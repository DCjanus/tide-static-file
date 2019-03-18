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
        actual_range, metadata, resolve_path, ErrorResponse, BOUNDARY, MULTI_RANGE_CONTENT_TYPE,
    },
};
use futures::future::FutureObj;
use http::{
    header::{self, HeaderValue},
    StatusCode,
};
use log::error;
use range_header::ByteRange;
use std::{
    ops::Range,
    path::{Path, PathBuf},
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

        let ranges = req
            .headers()
            .get(http::header::RANGE)
            .and_then(|x: &HeaderValue| x.to_str().ok())
            .map(ByteRange::parse);

        FutureObj::new(Box::new(async move { Self::run(target_path, ranges) }))
    }
}

impl StaticFiles {
    fn run(target_path: Option<PathBuf>, ranges: Option<Vec<ByteRange>>) -> Response {
        let target_path = match target_path {
            None => return ErrorResponse::NotFound.into_response(),
            Some(x) => x,
        };

        let (file, mime, file_size, last_modify) = match metadata(&target_path) {
            Err(_) => return ErrorResponse::Unexpected.into_response(),
            Ok(x) => x,
        };

        let timestamp = match last_modify.duration_since(std::time::UNIX_EPOCH) {
            Ok(x) => x.as_secs(),
            Err(error) => {
                error!("unexpected error occurred: {:?}", error);
                return ErrorResponse::Unexpected.into_response();
            }
        };
        let etag = format!("{:x}-{:x}", timestamp, file_size);

        let mime_text: &str = &mime.to_string();

        let ranges: Vec<ByteRange> = match ranges {
            None => {
                // no 'Range' header found
                let reader = match SingleRangeReader::new(file, 0, file_size) {
                    Ok(x) => x,
                    Err(error) => {
                        error!("unexpected error occurred: {:?}", error);
                        return ErrorResponse::Unexpected.into_response();
                    }
                };

                // whole file response
                return http::Response::builder()
                    .status(StatusCode::OK)
                    .header(header::ETAG, etag)
                    .header(header::CONTENT_TYPE, mime_text)
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_LENGTH, file_size)
                    .header(header::LAST_MODIFIED, httpdate::fmt_http_date(last_modify))
                    .body(reader.into_body())
                    .unwrap();
            }
            Some(x) => x,
        };

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

                http::Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::ETAG, etag)
                    .header(header::CONTENT_TYPE, mime_text)
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_RANGE, content_range_value)
                    .header(header::CONTENT_LENGTH, range.end - range.start)
                    .header(header::LAST_MODIFIED, httpdate::fmt_http_date(last_modify))
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

                http::Response::builder()
                    .status(http::StatusCode::PARTIAL_CONTENT)
                    .header(header::ETAG, etag)
                    .header(header::CONTENT_TYPE, MULTI_RANGE_CONTENT_TYPE)
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_LENGTH, content_length)
                    .header(header::LAST_MODIFIED, httpdate::fmt_http_date(last_modify))
                    .body(reader.into_body())
                    .unwrap()
            }
        }
    }
}
