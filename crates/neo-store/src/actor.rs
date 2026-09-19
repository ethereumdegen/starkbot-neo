use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use rusqlite::{Connection, OpenFlags};

use crate::{Result, StoreError, configure_connection};

type WriteJob = Box<dyn FnOnce(&mut Connection) + Send + 'static>;
type ReadJob = Box<dyn FnOnce(&Connection) + Send + 'static>;

#[derive(Clone)]
pub struct Writer {
    sender: mpsc::Sender<WriteJob>,
}

impl Writer {
    pub(crate) fn spawn(path: &Path) -> Result<Self> {
        let (sender, receiver) = mpsc::channel::<WriteJob>();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let path = path.to_owned();
        std::thread::Builder::new()
            .name("neo-db-writer".into())
            .spawn(move || {
                let connection =
                    Connection::open(path)
                        .map_err(StoreError::from)
                        .and_then(|connection| {
                            configure_connection(&connection, false)?;
                            Ok(connection)
                        });
                match connection {
                    Ok(mut connection) => {
                        let _ = ready_sender.send(Ok(()));
                        while let Ok(job) = receiver.recv() {
                            job(&mut connection);
                        }
                    }
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                    }
                }
            })
            .map_err(StoreError::Thread)?;
        ready_receiver
            .recv()
            .map_err(|_| StoreError::ActorStopped("writer"))??;
        Ok(Self { sender })
    }

    pub fn execute<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let (result_sender, result_receiver) = mpsc::sync_channel(1);
        self.sender
            .send(Box::new(move |connection| {
                let _ = result_sender.send(operation(connection));
            }))
            .map_err(|_| StoreError::ActorStopped("writer"))?;
        result_receiver
            .recv()
            .map_err(|_| StoreError::ActorStopped("writer"))?
    }
}

struct ReadPoolInner {
    senders: Vec<mpsc::Sender<ReadJob>>,
    next: AtomicUsize,
}

#[derive(Clone)]
pub struct ReadPool {
    inner: Arc<ReadPoolInner>,
}

impl ReadPool {
    pub(crate) fn spawn(path: &Path, size: usize) -> Result<Self> {
        if size == 0 {
            return Err(StoreError::InvalidPoolSize);
        }
        let mut senders = Vec::with_capacity(size);
        for index in 0..size {
            let (sender, receiver) = mpsc::channel::<ReadJob>();
            let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
            let path = path.to_owned();
            std::thread::Builder::new()
                .name(format!("neo-db-reader-{index}"))
                .spawn(move || {
                    let connection = Connection::open_with_flags(
                        path,
                        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                    )
                    .map_err(StoreError::from)
                    .and_then(|connection| {
                        configure_connection(&connection, true)?;
                        Ok(connection)
                    });
                    match connection {
                        Ok(connection) => {
                            let _ = ready_sender.send(Ok(()));
                            while let Ok(job) = receiver.recv() {
                                job(&connection);
                            }
                        }
                        Err(error) => {
                            let _ = ready_sender.send(Err(error));
                        }
                    }
                })
                .map_err(StoreError::Thread)?;
            ready_receiver
                .recv()
                .map_err(|_| StoreError::ActorStopped("reader"))??;
            senders.push(sender);
        }
        Ok(Self {
            inner: Arc::new(ReadPoolInner {
                senders,
                next: AtomicUsize::new(0),
            }),
        })
    }

    pub fn read<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let index = self.inner.next.fetch_add(1, Ordering::Relaxed) % self.inner.senders.len();
        let (result_sender, result_receiver) = mpsc::sync_channel(1);
        self.inner.senders[index]
            .send(Box::new(move |connection| {
                let _ = result_sender.send(operation(connection));
            }))
            .map_err(|_| StoreError::ActorStopped("reader"))?;
        result_receiver
            .recv()
            .map_err(|_| StoreError::ActorStopped("reader"))?
    }
}
