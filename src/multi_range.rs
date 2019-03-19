use crate::utils::{buffer_size, u64_width, BOUNDARY, MAX_BUFFER_SIZE};
use bytes::{buf::BufMut, Bytes};
use futures::{task::Waker, Poll, Stream};
use log::error;
use std::{
    collections::vec_deque::VecDeque,
    fs::File,
    io::{Cursor, Read, Seek, SeekFrom},
    ops::Range,
    pin::Pin,
};
const HEADER_SIZE_CONSTANT: usize = 56; // see the unit test for the actual meaning.

pub(super) struct MultiRangeReader {
    file: File,
    file_size: u64,
    mime: String,
    ranges: VecDeque<Range<u64>>,
    state: ToBeWritten,
}

#[derive(Eq, PartialEq, Debug)]
enum ToBeWritten {
    Header,
    Body,
    Final,
    None,
}

impl MultiRangeReader {
    pub fn new(file: File, file_size: u64, mime: &str, ranges: Vec<Range<u64>>) -> Self {
        if ranges.len() < 2 {
            unreachable!()
        }
        Self {
            file,
            file_size,
            mime: mime.to_string(),
            ranges: ranges.into(),
            state: ToBeWritten::Header,
        }
    }

    pub fn into_body(self) -> http_service::Body {
        http_service::Body::from_stream(self)
    }
}

impl Stream for MultiRangeReader {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, _: &Waker) -> Poll<Option<Self::Item>> {
        fn make_buffer(buffer: Cursor<Vec<u8>>) -> Result<Bytes, std::io::Error> {
            let position = buffer.position();
            if position == 0 {
                error!("unexpected error occurred: stream item length is 0");
                return Err(std::io::ErrorKind::Other.into());
            }

            let mut inner = buffer.into_inner();
            inner.truncate(position as usize);
            Ok(inner.into())
        }

        let mut buffer = Cursor::new(vec![0u8; MAX_BUFFER_SIZE]); // XXX to be improved
        loop {
            match self.state {
                ToBeWritten::Header => {
                    let first_range = self.ranges.front().unwrap();
                    let part_header = PartHeader::new(first_range, &self.mime, self.file_size);
                    if part_header.size() <= buffer.remaining_mut() {
                        part_header.write(&mut buffer);
                        self.state = ToBeWritten::Body;
                        continue;
                    } else {
                        // no enough room
                        return Poll::Ready(Some(make_buffer(buffer)));
                    }
                }
                ToBeWritten::Body => {
                    let mut first_range = self.ranges.pop_front().unwrap();
                    let remain = first_range.end - first_range.start;
                    let slice_size = buffer_size(remain, buffer.remaining_mut());
                    let slice_start = buffer.position() as usize;
                    let slice_end = slice_start + slice_size;
                    let slice = &mut buffer.get_mut()[slice_start..slice_end];

                    if let Err(error) = self.file.seek(SeekFrom::Start(first_range.start)) {
                        return Poll::Ready(Some(Err(error)));
                    }
                    let chunk_size = match self.file.read(slice) {
                        Ok(x) => x,
                        Err(error) => {
                            return Poll::Ready(Some(Err(error)));
                        }
                    };

                    first_range.start += chunk_size as u64;
                    buffer.set_position((slice_start + chunk_size) as u64);

                    debug_assert!(first_range.start <= first_range.end);
                    if first_range.start == first_range.end {
                        // this part has been completed
                        self.state = match self.ranges.len() {
                            0 => ToBeWritten::Final, // all parts has been completed
                            _ => ToBeWritten::Header,
                        };
                        continue;
                    } else {
                        self.ranges.push_front(first_range);
                        return Poll::Ready(Some(make_buffer(buffer)));
                    }
                }
                ToBeWritten::Final => {
                    if BOUNDARY.len() + 8 <= buffer.remaining_mut() {
                        use std::io::Write;
                        let write_result = write!(buffer, "\r\n--{}--\r\n", BOUNDARY);
                        if let Err(error) = write_result {
                            error!("unexpected error occurred: {}", error);
                            return Poll::Ready(Some(Err(error)));
                        }
                        self.state = ToBeWritten::None;
                    } else {
                        // do nothing
                    }
                    return Poll::Ready(Some(make_buffer(buffer)));
                }

                ToBeWritten::None => {
                    if buffer.position() == 0 {
                        return Poll::Ready(None);
                    } else {
                        return Poll::Ready(Some(make_buffer(buffer)));
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct PartHeader<'a> {
    range: &'a Range<u64>,
    mime_text: &'a str,
    total: u64,
}

impl<'a> PartHeader<'a> {
    pub fn new(range: &'a Range<u64>, mime_text: &'a str, total: u64) -> PartHeader<'a> {
        Self {
            range,
            mime_text,
            total,
        }
    }

    /// Calculate the space occupied by the part header.
    /// The part header will be constructed in memory, so the return value type is `usize`.
    pub fn size(&self) -> usize {
        HEADER_SIZE_CONSTANT
            + self.mime_text.len()
            + u64_width(self.range.start)
            + u64_width(self.range.end - 1)
            + u64_width(self.total)
    }

    /// Write part header into buffer
    pub fn write(&self, buffer: &mut std::io::Write) {
        let content_type = "content-type";
        let content_range = "content-range";

        #[allow(clippy::borrow_interior_mutable_const)]
        {
            debug_assert_eq!(content_type, http::header::CONTENT_TYPE.as_str());
            debug_assert_eq!(content_range, http::header::CONTENT_RANGE.as_str());
        }

        write!(buffer, "\r\n--{boundary}\r\n{content_type}: {mime}\r\n{content_range}: bytes {start}-{end}/{total}\r\n\r\n",
               content_type =content_type,
               mime = self.mime_text,
               content_range = content_range,
               total = self.total,
               end = self.range.end - 1,
               start = self.range.start,
               boundary = BOUNDARY,
        ).expect("unexpected error occupied when constructing part header");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header;

    #[test]
    fn test_part_header_size_constant() {
        // with feature `const_str_len`, this unit test will no longer be needed
        let expected = "\r\n".len() +
            "--".len() + BOUNDARY.len() + "\r\n".len() +
            header::CONTENT_TYPE.as_str().len() + ": ".len() + /* mime.len() + */"\r\n".len() +
            header::CONTENT_RANGE.as_str().len() + ": ".len() + "bytes ".len() + /* u64_width(range.start) + */ "-".len() + /* u64_width(range.end) + */"/".len() + /* u64_width(total) + */"\r\n".len() +
            "\r\n".len();

        assert_eq!(HEADER_SIZE_CONSTANT, expected)
    }

    #[test]
    fn test_part_header_size() {
        let test_case = [
            (
                mime::TEXT_PLAIN_UTF_8.as_ref(),
                &Range {
                    start: 2u64,
                    end: 100u64,
                },
                1000,
            ),
            (
                mime::CSS.as_ref(),
                &Range {
                    start: 0u64,
                    end: 100u64,
                },
                111,
            ),
        ];
        for i in &test_case {
            let mut buffer = Cursor::new(vec![0u8; MAX_BUFFER_SIZE]);
            let header = PartHeader::new(i.1, i.0, i.2);
            header.write(&mut buffer);
            assert_eq!(header.size(), buffer.position() as usize);
        }
    }
}
