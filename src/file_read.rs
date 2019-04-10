use crate::utils::{buffer_size, MAX_BUFFER_SIZE};
use bytes::{Bytes, BytesMut};
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use futures::io::ErrorKind;
use lazy_static::lazy_static;
use std::{
    fs::File,
    io::{Error as IoError, Read, Seek, SeekFrom},
    ops::Range,
    sync::{Arc, Mutex},
    task::{Poll, Waker},
};

pub(crate) struct FileReadStream {
    range: Range<u64>,
    state: StreamState,
}

impl FileReadStream {
    pub fn new(mut file: File, range: Range<u64>) -> Result<Self, (File, IoError)> {
        assert!(range.start <= range.end);
        if let Err(error) = file.seek(SeekFrom::Start(range.start)) {
            return Err((file, error));
        }
        Ok(Self {
            range,
            state: StreamState::Init(file),
        })
    }

    pub fn poll_next(&mut self, waker: &Waker) -> StreamOutput {
        assert!(self.range.start <= self.range.end);
        if self.range.start == self.range.end {
            return StreamOutput::Complete(self.state.get_file().unwrap());
        }

        if let Some(file) = self.state.get_file() {
            let buffer_size = buffer_size(self.range.end - self.range.start, MAX_BUFFER_SIZE);
            let buffer = BytesMut::from(vec![0u8; buffer_size]);
            let task = match FileReadTask::create(file, buffer) {
                Ok(x) => x,
                Err(_) => return StreamOutput::Error(ErrorKind::WouldBlock.into()),
            };
            self.state.put_task(task);
        }

        let task = self.state.get_task().unwrap();
        match task.poll(waker) {
            Poll::Ready(Ok((file, bytes))) => {
                self.range.start += bytes.len() as u64;
                self.state.put_file(file);
                StreamOutput::Item(bytes)
            }
            Poll::Ready(Err((_, _, error))) => StreamOutput::Error(error),
            Poll::Pending => {
                self.state.put_task(task);
                StreamOutput::Pending
            }
        }
    }
}

enum StreamState {
    Init(File),
    Work(FileReadTask),
    Temp,
}

impl StreamState {
    fn get_file(&mut self) -> Option<File> {
        if let StreamState::Init(_) = self {
            if let StreamState::Init(file) = ::std::mem::replace(self, StreamState::Temp) {
                Some(file)
            } else {
                unreachable!()
            }
        } else {
            None
        }
    }

    fn put_file(&mut self, file: File) {
        *self = StreamState::Init(file);
    }

    fn get_task(&mut self) -> Option<FileReadTask> {
        if let StreamState::Work(_) = self {
            if let StreamState::Work(task) = ::std::mem::replace(self, StreamState::Temp) {
                Some(task)
            } else {
                unreachable!()
            }
        } else {
            None
        }
    }

    fn put_task(&mut self, task: FileReadTask) {
        *self = StreamState::Work(task);
    }
}

pub enum StreamOutput {
    Pending,
    Error(IoError),
    Item(Bytes),
    Complete(File),
}

#[derive(Clone)]
struct FileReadTask {
    state: Arc<Mutex<TaskState>>,
}

impl FileReadTask {
    pub fn create(file: File, buffer: BytesMut) -> Result<Self, (File, BytesMut)> {
        lazy_static! {
            static ref SENDER: Sender<FileReadTask> = {
                let (sender, receiver) = bounded(1024);
                for _ in 0..8 {
                    let receiver = receiver.clone();
                    ::std::thread::spawn(|| worker(receiver));
                }
                sender
            };
        }

        let task = FileReadTask {
            state: Arc::new(Mutex::new(TaskState::Init(file, buffer))),
        };
        match SENDER.try_send(task.clone()) {
            Ok(_) => Ok(task),
            Err(TrySendError::Full(_)) => match task.state.lock().unwrap().get_state() {
                TaskState::Init(file, buffer) => Err((file, buffer)),
                _ => unreachable!(),
            },
            Err(TrySendError::Disconnected(_)) => unreachable!(),
        }
    }

    #[allow(clippy::type_complexity)]
    pub fn poll(&self, waker: &Waker) -> Poll<Result<(File, Bytes), (File, BytesMut, IoError)>> {
        let mut guard = self.state.lock().unwrap();
        match guard.get_state() {
            TaskState::Init(file, buffer) => {
                guard.put_state(TaskState::Ready(file, buffer, waker.clone()));
                Poll::Pending
            }
            TaskState::WaitWaker => {
                guard.put_state(TaskState::SendWaker(waker.clone()));
                Poll::Pending
            }
            TaskState::Done(result) => Poll::Ready(result),
            TaskState::Temp
            | TaskState::Working
            | TaskState::SendWaker(_)
            | TaskState::Ready(_, _, _) => Poll::Pending,
        }
    }
}

fn worker(receiver: Receiver<FileReadTask>) {
    for task in receiver {
        let mut guard = task.state.lock().unwrap();
        let (mut file, mut buffer, waker) = match guard.get_state() {
            TaskState::Init(file, buffer) => {
                guard.put_state(TaskState::WaitWaker);
                (file, buffer, None)
            }
            TaskState::Ready(file, buffer, waker) => {
                guard.put_state(TaskState::Working);
                (file, buffer, Some(waker))
            }
            TaskState::WaitWaker
            | TaskState::SendWaker(_)
            | TaskState::Working
            | TaskState::Done(_)
            | TaskState::Temp => unreachable!(),
        };
        drop(guard);

        let read_result = match file.read(&mut buffer) {
            Ok(size) => {
                buffer.truncate(size);
                Ok((file, buffer.freeze()))
            }
            Err(error) => Err((file, buffer, error)),
        };

        let mut guard = task.state.lock().unwrap();
        match guard.get_state() {
            TaskState::WaitWaker => guard.put_state(TaskState::Done(read_result)),
            TaskState::SendWaker(waker) => {
                guard.put_state(TaskState::Done(read_result));
                waker.wake();
            }
            TaskState::Working => {
                guard.put_state(TaskState::Done(read_result));
                waker.unwrap().wake();
            }
            TaskState::Ready(_, _, _)
            | TaskState::Done(_)
            | TaskState::Init(_, _)
            | TaskState::Temp => unreachable!(),
        }
        drop(guard);
    }
}

#[derive(Debug)]
enum TaskState {
    Init(File, BytesMut),
    Ready(File, BytesMut, Waker),

    WaitWaker,
    SendWaker(Waker),

    Working,
    Done(Result<(File, Bytes), (File, BytesMut, std::io::Error)>),

    Temp,
}

impl TaskState {
    fn get_state(&mut self) -> Self {
        let result = ::std::mem::replace(self, TaskState::Temp);
        if let TaskState::Temp = result {
            unreachable!()
        } else {
            result
        }
    }

    fn put_state(&mut self, state: Self) {
        if let TaskState::Temp = std::mem::replace(self, state) {
        } else {
            unreachable!()
        }
    }
}
