use crate::error::TSFResult;
use http::{header, StatusCode};
use mime::Mime;
use range_header::ByteRange;
use std::{
    cmp::min,
    fs::File,
    ops::Range,
    path::{Path, PathBuf},
    time::SystemTime,
};
use tide::{IntoResponse, Response};

pub(crate) const MAX_BUFFER_SIZE: usize = 1024 * 1024 * 4;
pub(crate) const BOUNDARY: &str = "DCjanus";
pub(crate) const MULTI_RANGE_CONTENT_TYPE: &str = "multipart/byteranges; boundary=DCjanus";

pub(crate) enum ErrorResponse {
    NotFound,
    Unexpected,
}

impl IntoResponse for ErrorResponse {
    fn into_response(self) -> Response {
        match self {
            ErrorResponse::NotFound => http::Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(header::CONTENT_TYPE, mime::TEXT_PLAIN.to_string())
                .body("not found".into())
                .unwrap(),
            ErrorResponse::Unexpected => http::Response::builder()
                .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                .header(header::CONTENT_TYPE, mime::TEXT_PLAIN.to_string())
                .body("unexpected error occurred".into())
                .unwrap(),
        }
    }
}

pub(crate) fn resolve_path(root: &Path, url_path: &str) -> PathBuf {
    let mut p = PathBuf::new();
    for i in url_path.split(|c| c == '/' || c == '\\') {
        match i {
            "." => {
                continue;
            }
            ".." => {
                p.pop();
            }
            x => {
                p.push(x);
            }
        }
    }
    root.join(p)
}

pub(crate) fn metadata(path: &Path) -> TSFResult<(File, Mime, u64, SystemTime)> {
    let mime = mime_guess::guess_mime_type(&path);
    let file = File::open(path)?;
    let meta = file.metadata()?;
    let size = meta.len();
    let last_modify = meta.modified()?;

    Ok((file, mime, size, last_modify))
}

pub(crate) fn actual_range(byte_range: ByteRange, file_size: u64) -> Option<Range<u64>> {
    match byte_range {
        ByteRange::FromTo(start) => {
            if start < file_size {
                Some(Range {
                    start,
                    end: file_size,
                })
            } else {
                None
            }
        }
        ByteRange::FromToAll(start, end) => {
            if start <= end && start < file_size {
                Some(Range {
                    start,
                    end: min(file_size, end + 1),
                })
            } else {
                None
            }
        }
        ByteRange::Last(length) => {
            if length > 0 {
                Some(Range {
                    start: file_size.saturating_sub(length),
                    end: file_size,
                })
            } else {
                None
            }
        }
    }
}

pub(crate) fn buffer_size(remain: u64, max_buffer_size: usize) -> usize {
    if remain > usize::max_value() as u64 {
        max_buffer_size
    } else {
        min(remain as usize, max_buffer_size)
    }
}

/// given number `x`, return `x.to_string().len()`
#[inline]
#[allow(clippy::unreadable_literal)]
pub(super) fn u64_width(x: u64) -> usize {
    const NUMBERS: [u64; 19] = [
        10,
        100,
        1000,
        10000,
        100000,
        1000000,
        10000000,
        100000000,
        1000000000,
        10000000000,
        100000000000,
        1000000000000,
        10000000000000,
        100000000000000,
        1000000000000000,
        10000000000000000,
        100000000000000000,
        1000000000000000000,
        10000000000000000000,
    ];
    NUMBERS.iter().position(|limit| *limit > x).unwrap_or(19) + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn test_constraints() {
        assert!(size_of::<usize>() <= size_of::<u64>());
        assert!(size_of::<usize>() >= size_of::<u32>());
        assert_eq!(
            MULTI_RANGE_CONTENT_TYPE,
            format!("multipart/byteranges; boundary={}", BOUNDARY)
        );
    }

    #[test]
    fn test_resolve_path() {
        let base_dir = &PathBuf::from("/virtual");
        assert_eq!(resolve_path(base_dir, "foo"), PathBuf::from("/virtual/foo"));
        assert_eq!(
            resolve_path(base_dir, "/foo"),
            PathBuf::from("/virtual/foo")
        );
        assert_eq!(
            resolve_path(base_dir, "////foo"),
            PathBuf::from("/virtual/foo")
        );
        assert_eq!(
            resolve_path(base_dir, "../foo"),
            PathBuf::from("/virtual/foo")
        );
        assert_eq!(resolve_path(base_dir, "foo/.."), PathBuf::from("/virtual"));
        assert_eq!(
            resolve_path(base_dir, "foo/../other"),
            PathBuf::from("/virtual/other")
        );
    }

    #[test]
    fn test_actual_range() {
        assert_eq!(
            Some(Range {
                start: 100,
                end: 101,
            }),
            actual_range(ByteRange::FromToAll(100, 100), 200)
        );
        assert_eq!(None, actual_range(ByteRange::FromToAll(100, 100), 100));
        assert_eq!(None, actual_range(ByteRange::FromToAll(10, 1), 100));

        assert_eq!(
            Some(Range {
                start: 100,
                end: 200,
            }),
            actual_range(ByteRange::FromToAll(100, 199), 200)
        );
        assert_eq!(
            Some(Range {
                start: 100,
                end: 200,
            }),
            actual_range(ByteRange::FromTo(100), 200)
        );
        assert_eq!(
            Some(Range {
                start: 100,
                end: 200,
            }),
            actual_range(ByteRange::Last(100), 200)
        );
    }

    #[test]
    fn test_buffer_size() {
        assert_eq!(0, buffer_size(0, MAX_BUFFER_SIZE));
        assert_eq!(
            MAX_BUFFER_SIZE,
            buffer_size(MAX_BUFFER_SIZE as u64 + 1, MAX_BUFFER_SIZE)
        );
    }

    #[test]
    fn test_width() {
        let test_case = [0, 9, 10, 99, 100, u64::max_value()];
        for &i in test_case.into_iter() {
            assert_eq!(i.to_string().len(), u64_width(i));
        }
    }
}
