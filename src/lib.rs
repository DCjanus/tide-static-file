#![feature(async_await, await_macro, futures_api)]

//! Static file server implementation, work with [Tide](https://github.com/rustasync/tide)

mod error;
mod multi_range;
mod single_range;
mod utils;

pub use crate::error::TSFResult;
use crate::{
    multi_range::{MultiRangeReader, PartHeader},
    single_range::SingleRangeReader,
    utils::{
        actual_range, get_header, metadata, resolve_path, ErrorResponse, BOUNDARY,
        MULTI_RANGE_CONTENT_TYPE,
    },
};
use futures::future::FutureObj;
use http::{
    header::{self, HeaderValue},
    StatusCode,
};
use http_service::Body;
use httpdate::HttpDate;
use log::error;
use range_header::ByteRange;
use std::{
    fs::File,
    ops::Range,
    path::{Path, PathBuf},
    time::SystemTime,
};
use tide::{configuration::Store, IntoResponse, Request, Response, RouteMatch};

pub struct StaticFiles {
    root: PathBuf,
}

impl StaticFiles {
    pub fn new(root: impl AsRef<Path>) -> TSFResult<Self> {
        let root = root.as_ref().to_path_buf();
        if !root.is_dir() {
            return Err(error::NoSuchDirectory(root).into());
        }
        Ok(Self {
            root: root
                .canonicalize()
                .map_err(|_| error::NoSuchDirectory(root))?,
        })
    }
}

impl<Data> tide::Endpoint<Data, ()> for StaticFiles {
    type Fut = FutureObj<'static, Response>;

    fn call(&self, _: Data, req: Request, params: Option<RouteMatch<'_>>, _: &Store) -> Self::Fut {
        let target_path = params
            .and_then(|rm| rm.vec.first().map(|x| resolve_path(&self.root, x)))
            .and_then(|x| x.canonicalize().ok());

        FutureObj::new(Box::new(async move { Self::run(target_path, req) }))
    }
}

impl StaticFiles {
    fn run(target_path: Option<PathBuf>, req: Request) -> Response {
        // TODO this function is too long

        let target_path = match target_path {
            None => return ErrorResponse::NotFound.into_response(),
            Some(x) => x,
        };
        let (file, mime, file_size, last_modified, etag) = match metadata(&target_path) {
            Err(error) => {
                error!("unexpected error occurred: {:?}", error);
                return ErrorResponse::Unexpected.into_response();
            }
            Ok(x) => x,
        };
        let mime_text: &str = &mime.to_string();

        let mut common_response = http::Response::builder();
        common_response
            .header(header::ETAG, etag.clone())
            .header(header::ACCEPT_RANGES, "bytes")
            .header(
                header::LAST_MODIFIED,
                httpdate::fmt_http_date(last_modified),
            );

        let should_cache = Self::should_cache(
            get_header(&req, http::header::IF_MODIFIED_SINCE),
            get_header(&req, http::header::IF_NONE_MATCH),
            last_modified,
            &etag,
        );
        if should_cache {
            return common_response
                .status(StatusCode::NOT_MODIFIED)
                .body(Body::empty())
                .unwrap();
        }

        let should_range = Self::should_range(
            get_header(&req, http::header::IF_RANGE),
            &etag,
            last_modified,
        );
        if !should_range {
            return Self::whole_file_response(common_response, file, file_size, mime_text);
        }

        let ranges: Option<Vec<ByteRange>> = req
            .headers()
            .get(http::header::RANGE)
            .and_then(|x: &HeaderValue| x.to_str().ok())
            .map(ByteRange::parse);
        if ranges.is_none() {
            return Self::whole_file_response(common_response, file, file_size, mime_text);
        }

        let ranges: Vec<ByteRange> = ranges.unwrap();
        if ranges.is_empty() {
            // no valid (format) 'Range' header value found
            // for example: 'Range: lines=1-2' or 'Range: nothing'
            return http::Response::builder()
                .status(http::StatusCode::BAD_REQUEST)
                .header(header::CONTENT_TYPE, mime::TEXT_PLAIN.to_string())
                .header(header::ACCEPT_RANGES, "bytes")
                .body("failed to parse request header: Range".into())
                .unwrap();
        }

        // "redirects and failures take precedence over the evaluation of
        // preconditions in conditional requests."
        // ref: https://tools.ietf.org/html/rfc7232#section-5
        //
        // It's too hard to check all things
        // So we put precondition check here
        let should_precondition_failed = Self::precondition_failed(
            get_header(&req, http::header::IF_MATCH),
            get_header(&req, http::header::IF_UNMODIFIED_SINCE),
            last_modified,
            &etag,
        );
        if should_precondition_failed {
            return http::Response::builder()
                .status(http::StatusCode::PRECONDITION_FAILED)
                .header(header::CONTENT_TYPE, mime::TEXT_PLAIN.to_string())
                .header(header::ACCEPT_RANGES, "bytes")
                .body("precondition failed".into())
                .unwrap();
        }

        let mut ranges: Vec<Range<u64>> = ranges
            .into_iter()
            .flat_map(|x| actual_range(x, file_size))
            .collect();
        match ranges.len() {
            0 => {
                // no valid 'Range' header valid found
                // for example: file size is 200, got 'Range: bytes=400-'
                http::Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_TYPE, mime::TEXT_PLAIN_UTF_8.to_string())
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_RANGE, format!("bytes */{}", file_size))
                    .body("requested range not satisfiable".into())
                    .unwrap()
            }
            1 => {
                // only one valid 'Range' header found
                let range = ranges.pop().unwrap();
                let content_range_value = format!(
                    "bytes {start}-{end}/{total}",
                    start = range.start,
                    end = range.end - 1,
                    total = file_size
                );

                let reader = match SingleRangeReader::new(file, range.start, range.end) {
                    Ok(x) => x,
                    Err(error) => {
                        error!("unexpected error occurred: {:?}", error);
                        return ErrorResponse::Unexpected.into_response();
                    }
                };

                common_response
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_TYPE, mime_text)
                    .header(header::CONTENT_RANGE, content_range_value)
                    .header(header::CONTENT_LENGTH, range.end - range.start)
                    .body(reader.into_body())
                    .unwrap()
            }
            _ => {
                // multi valid 'Range' header found
                let header_length: usize = ranges
                    .iter()
                    .map(|x| PartHeader::new(x, mime_text, file_size).size())
                    .sum();
                let body_length: u64 = ranges.iter().map(|x| x.end - x.start).sum();
                let final_length = 8 + BOUNDARY.len(); /*"\r\n--".len() + BOUNDARY.len() + "--\r\n".len()*/
                let content_length = header_length as u64 + body_length + final_length as u64;

                let reader = MultiRangeReader::new(file, file_size, mime_text, ranges);

                common_response
                    .status(http::StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_TYPE, MULTI_RANGE_CONTENT_TYPE)
                    .header(header::CONTENT_LENGTH, content_length)
                    .body(reader.into_body())
                    .unwrap()
            }
        }
    }
}

impl StaticFiles {
    /// ref: https://tools.ietf.org/html/rfc7233#section-3.2
    pub(crate) fn should_range(
        if_range: Option<String>,
        etag: &str,
        last_modify: SystemTime,
    ) -> bool {
        if let Some(x) = if_range
            .as_ref()
            .and_then(|x| x.parse::<HttpDate>().ok())
            .map(|x| x == HttpDate::from(last_modify))
        {
            return x;
        }

        if let Some(x) = if_range.map(|x| x.split(',').map(str::trim).any(|x| x == etag)) {
            return x;
        }

        false
    }

    /// HTTP 304 (Not Modified) or not
    ///
    /// ref:
    /// + https://tools.ietf.org/html/rfc7232#section-3.2
    /// + https://tools.ietf.org/html/rfc7232#section-3.3
    pub(crate) fn should_cache(
        if_modified_since: Option<String>,
        if_none_match: Option<String>,
        last_modified: SystemTime,
        etag: &str,
    ) -> bool {
        if let Some(etags) = if_none_match {
            etags.split(',').map(str::trim).any(|x| x == etag)
        } else {
            if_modified_since
                .and_then(|x| x.parse::<HttpDate>().ok())
                .map(|x| x == HttpDate::from(last_modified))
                .unwrap_or(false)
        }
    }

    /// HTTP 412 (Precondition Failed) or not
    ///
    /// ref: https://tools.ietf.org/html/rfc7232#section-4.2
    pub(crate) fn precondition_failed(
        if_match: Option<String>,
        if_unmodified_since: Option<String>,
        last_modified: SystemTime,
        etag: &str,
    ) -> bool {
        if let Some(expect) = if_match {
            expect.split(',').map(str::trim).all(|x| x != etag)
        } else {
            if_unmodified_since
                .and_then(|x| x.parse::<HttpDate>().ok())
                .map(|x| x != HttpDate::from(last_modified))
                .unwrap_or(false)
        }
    }

    fn whole_file_response(
        mut common_response: http::response::Builder,
        file: File,
        file_size: u64,
        mime_text: &str,
    ) -> Response {
        let reader = match SingleRangeReader::new(file, 0, file_size) {
            Ok(x) => x,
            Err(error) => {
                error!("unexpected error occurred: {:?}", error);
                return ErrorResponse::Unexpected.into_response();
            }
        };

        common_response
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime_text)
            .header(header::CONTENT_LENGTH, file_size)
            .body(reader.into_body())
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::StaticFiles;
    use std::{
        ops::Add,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn test_should_cache() {
        let before = &UNIX_EPOCH;
        let before_text = &httpdate::fmt_http_date(before.clone());

        let little_diff = before.add(Duration::from_millis(1));
        let little_text = &httpdate::fmt_http_date(little_diff.clone());

        let after = &before.add(Duration::from_secs(10));
        let after_text = &httpdate::fmt_http_date(after.clone());

        assert_eq!(
            true,
            StaticFiles::should_cache(
                Some(before_text.to_owned()),
                None,
                before.clone(),
                "correct",
            )
        );
        assert_eq!(
            true,
            StaticFiles::should_cache(
                Some(little_text.to_owned()),
                None,
                before.clone(),
                "correct",
            )
        );
        assert_eq!(
            false,
            StaticFiles::should_cache(Some(after_text.to_owned()), None, before.clone(), "correct")
        );
        assert_eq!(
            false,
            StaticFiles::should_cache(Some(before_text.to_owned()), None, after.clone(), "correct")
        );
        assert_eq!(
            false,
            StaticFiles::should_cache(
                Some(after_text.to_owned()),
                Some("wrong".to_owned()),
                after.clone(),
                "correct",
            )
        );
        assert_eq!(
            true,
            StaticFiles::should_cache(
                Some(after_text.to_owned()),
                Some("wrong, correct ".to_owned()),
                after.clone(),
                "correct",
            )
        );
        assert_eq!(
            false,
            StaticFiles::should_cache(None, Some("wrong".to_owned()), after.clone(), "correct")
        );
        assert_eq!(
            true,
            StaticFiles::should_cache(
                Some(little_text.to_owned()),
                Some("correct".to_owned()),
                after.clone(),
                "correct",
            )
        );
    }

    #[test]
    fn test_precondition_failed() {
        let before = &UNIX_EPOCH;
        let before_text = &httpdate::fmt_http_date(before.clone());

        let little_diff = before.add(Duration::from_millis(1));
        let little_text = &httpdate::fmt_http_date(little_diff.clone());

        let after = &before.add(Duration::from_secs(10));
        let after_text = &httpdate::fmt_http_date(after.clone());

        assert_eq!(
            false,
            StaticFiles::precondition_failed(
                None,
                Some(before_text.to_owned()),
                before.clone(),
                "correct",
            )
        );
        assert_eq!(
            false,
            StaticFiles::precondition_failed(
                None,
                Some(little_text.to_owned()),
                before.clone(),
                "correct",
            )
        );
        assert_eq!(
            false,
            StaticFiles::precondition_failed(
                None,
                Some(before_text.to_owned()),
                little_diff.clone(),
                "correct",
            )
        );
        assert_eq!(
            true,
            StaticFiles::precondition_failed(
                None,
                Some(after_text.to_owned()),
                before.clone(),
                "correct",
            )
        );
        assert_eq!(
            true,
            StaticFiles::precondition_failed(
                None,
                Some(before_text.to_owned()),
                after.clone(),
                "correct",
            )
        );
        assert_eq!(
            false,
            StaticFiles::precondition_failed(
                Some("correct".to_owned()),
                Some(before_text.to_owned()),
                after.clone(),
                "correct",
            )
        );
        assert_eq!(
            false,
            StaticFiles::precondition_failed(
                Some("correct, wrong".to_owned()),
                Some(before_text.to_owned()),
                after.clone(),
                "correct",
            )
        );
        assert_eq!(
            true,
            StaticFiles::precondition_failed(
                Some("wrong".to_owned()),
                Some(before_text.to_owned()),
                after.clone(),
                "correct",
            )
        );
    }

    #[test]
    fn test_should_range() {
        let before = &UNIX_EPOCH;
        let before_text = &httpdate::fmt_http_date(before.clone());

        let little_diff = before.add(Duration::from_millis(1));
        let little_text = &httpdate::fmt_http_date(little_diff.clone());

        let after = &before.add(Duration::from_secs(10));
        let after_text = &httpdate::fmt_http_date(after.clone());

        assert_eq!(
            true,
            StaticFiles::should_range(Some(before_text.to_owned()), "correct", before.clone())
        );
        assert_eq!(
            true,
            StaticFiles::should_range(Some(little_text.to_owned()), "correct", before.clone())
        );
        assert_eq!(
            false,
            StaticFiles::should_range(Some(before_text.to_owned()), "correct", after.clone())
        );
        assert_eq!(
            false,
            StaticFiles::should_range(Some(after_text.to_owned()), "correct", before.clone())
        );
        assert_eq!(
            true,
            StaticFiles::should_range(Some("correct".to_owned()), "correct", before.clone()),
        );
        assert_eq!(
            false,
            StaticFiles::should_range(Some("wrong".to_owned()), "correct", before.clone()),
        );
        assert_eq!(
            true,
            StaticFiles::should_range(
                Some("wrong, correct ".to_owned()),
                "correct",
                before.clone(),
            ),
        );
    }
}
