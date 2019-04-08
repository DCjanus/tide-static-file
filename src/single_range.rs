use crate::{
    file_read::FileReadTask,
    utils::{buffer_size, MAX_BUFFER_SIZE},
};
use bytes::{Bytes, BytesMut};
use futures::{task::Waker, Poll, Stream};
use std::{
    fs::File,
    io::{Seek, SeekFrom},
    ops::Range,
    pin::Pin,
};

pub(super) struct SingleRangeReader {
    range: Range<u64>,
    task: FileReadTask,
}

impl SingleRangeReader {
    pub fn new(mut file: File, start: u64, end: u64) -> Result<Self, std::io::Error> {
        assert!(start < end);
        file.seek(SeekFrom::Start(start))?;
        let buffer_size = buffer_size(end - start, MAX_BUFFER_SIZE);
        let buffer = BytesMut::from(vec![0u8; buffer_size]);
        let task = match FileReadTask::create(file, buffer) {
            Ok(x) => x,
            Err(_) => return Err(std::io::ErrorKind::WouldBlock.into()),
        };

        Ok(Self {
            task,
            range: Range { start, end },
        })
    }

    pub fn into_body(self) -> http_service::Body {
        http_service::Body::from_stream(self)
    }
}

impl Stream for SingleRangeReader {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, waker: &Waker) -> Poll<Option<Self::Item>> {
        assert!(self.range.start <= self.range.end);
        if self.range.start == self.range.end {
            return Poll::Ready(None);
        }

        let (file, buffer) = match self.task.poll(waker) {
            Poll::Ready(Ok((file, buffer))) => (file, buffer),
            Poll::Ready(Err((_, _, error))) => return Poll::Ready(Some(Err(error))),
            Poll::Pending => return Poll::Pending,
        };

        self.range.start += buffer.len() as u64;
        assert!(self.range.start <= self.range.end);
        let buffer_size = buffer_size(self.range.end - self.range.start, MAX_BUFFER_SIZE);
        if buffer_size > 0 {
            let buffer = BytesMut::from(vec![0u8; buffer_size]);
            self.task = match FileReadTask::create(file, buffer) {
                Ok(x) => x,
                Err(_) => return Poll::Ready(Some(Err(std::io::ErrorKind::WouldBlock.into()))),
            };
        }

        Poll::Ready(Some(Ok(buffer)))
    }
}
