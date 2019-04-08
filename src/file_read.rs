use bytes::{Bytes, BytesMut};
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use lazy_static::lazy_static;
use std::{
    fs::File,
    io::Read,
    sync::{Arc, Mutex},
    task::{Poll, Waker},
};

#[derive(Clone)]
pub(crate) struct FileReadTask {
    state: Arc<Mutex<State>>,
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
            state: Arc::new(Mutex::new(State::Init(file, buffer))),
        };
        match SENDER.try_send(task.clone()) {
            Ok(_) => Ok(task),
            Err(TrySendError::Full(_)) => match task.state.lock().unwrap().get_state() {
                State::Init(file, buffer) => Err((file, buffer)),
                _ => unreachable!(),
            },
            Err(TrySendError::Disconnected(_)) => unreachable!(),
        }
    }

    #[allow(clippy::type_complexity)]
    pub fn poll(
        &self,
        waker: &Waker,
    ) -> Poll<Result<(File, Bytes), (File, BytesMut, std::io::Error)>> {
        let mut guard = self.state.lock().unwrap();
        match guard.get_state() {
            State::Init(file, buffer) => {
                guard.put_state(State::Ready(file, buffer, waker.clone()));
                Poll::Pending
            }
            State::WaitWaker => {
                guard.put_state(State::SendWaker(waker.clone()));
                Poll::Pending
            }
            State::Done(result) => Poll::Ready(result),
            State::Temp | State::Working | State::SendWaker(_) | State::Ready(_, _, _) => {
                Poll::Pending
            }
        }
    }
}

fn worker(receiver: Receiver<FileReadTask>) {
    for task in receiver {
        let mut guard = task.state.lock().unwrap();
        let (mut file, mut buffer, waker) = match guard.get_state() {
            State::Init(file, buffer) => {
                guard.put_state(State::WaitWaker);
                (file, buffer, None)
            }
            State::Ready(file, buffer, waker) => {
                guard.put_state(State::Working);
                (file, buffer, Some(waker))
            }
            State::WaitWaker
            | State::SendWaker(_)
            | State::Working
            | State::Done(_)
            | State::Temp => unreachable!(),
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
            State::WaitWaker => guard.put_state(State::Done(read_result)),
            State::SendWaker(waker) => {
                guard.put_state(State::Done(read_result));
                waker.wake();
            }
            State::Working => {
                guard.put_state(State::Done(read_result));
                waker.unwrap().wake();
            }
            State::Ready(_, _, _) | State::Done(_) | State::Init(_, _) | State::Temp => {
                unreachable!()
            }
        }
        drop(guard);
    }
}

#[derive(Debug)]
enum State {
    Init(File, BytesMut),
    Ready(File, BytesMut, Waker),

    WaitWaker,
    SendWaker(Waker),

    Working,
    Done(Result<(File, Bytes), (File, BytesMut, std::io::Error)>),

    Temp,
}

impl State {
    fn get_state(&mut self) -> Self {
        let result = ::std::mem::replace(self, State::Temp);
        if let State::Temp = result {
            unreachable!()
        } else {
            result
        }
    }

    fn put_state(&mut self, state: Self) {
        if let State::Temp = std::mem::replace(self, state) {
        } else {
            unreachable!()
        }
    }
}
