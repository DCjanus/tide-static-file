use crate::file_read::{FileReadStream, StreamOutput};
use bytes::Bytes;
use futures::{task::Waker, Poll, Stream};
use std::{fs::File, ops::Range, pin::Pin};

pub(super) struct SingleRangeReader {
    reader: FileReadStream,
}

impl SingleRangeReader {
    pub fn new(file: File, start: u64, end: u64) -> Result<Self, std::io::Error> {
        assert!(start < end);
        let reader = match FileReadStream::new(file, Range { start, end }) {
            Ok(x) => x,
            Err((_, error)) => return Err(error),
        };
        Ok(Self { reader })
    }

    pub fn into_body(self) -> http_service::Body {
        http_service::Body::from_stream(self)
    }
}

impl Stream for SingleRangeReader {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, waker: &Waker) -> Poll<Option<Self::Item>> {
        match self.reader.poll_next(waker) {
            StreamOutput::Pending => Poll::Pending,
            StreamOutput::Error(error) => Poll::Ready(Some(Err(error))),
            StreamOutput::Item(data) => Poll::Ready(Some(Ok(data))),
            StreamOutput::Complete(_) => Poll::Ready(None),
        }
    }
}
