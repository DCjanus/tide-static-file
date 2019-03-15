use crate::utils::{buffer_size, MAX_BUFFER_SIZE};
use bytes::Bytes;
use futures::{task::Waker, Poll, Stream};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    ops::Range,
    pin::Pin,
};

pub(super) struct SingleRangeReader {
    file: File,
    range: Range<u64>,
}

impl SingleRangeReader {
    pub fn new(mut file: File, start: u64, end: u64) -> Result<Self, std::io::Error> {
        if start >= end {
            unreachable!()
        }

        file.seek(SeekFrom::Start(start))?;
        Ok(Self {
            file,
            range: Range { start, end },
        })
    }

    pub fn into_body(self) -> http_service::Body {
        http_service::Body::from_stream(self)
    }
}

impl Stream for SingleRangeReader {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, _: &Waker) -> Poll<Option<Self::Item>> {
        debug_assert!(self.range.end > self.range.start);
        let buffer_size = buffer_size(self.range.end - self.range.start, MAX_BUFFER_SIZE);
        if buffer_size == 0 {
            return Poll::Ready(None);
        }
        let mut buffer = vec![0u8; buffer_size];
        match self.file.read(&mut buffer) {
            Ok(size) => {
                self.range.start += size as u64;
                buffer.truncate(size);
                Poll::Ready(Some(Ok(buffer.into())))
            }
            Err(error) => Poll::Ready(Some(Err(error))),
        }
    }
}
