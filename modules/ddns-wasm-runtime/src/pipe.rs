use std::any::Any;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::{Arc, Weak};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use tokio::sync::oneshot;
use wasi_common::file::{FdFlags, FileType};
use wasi_common::{Error, WasiFile};
use super::SharedCtx;

#[derive(Debug)]
pub struct ReadWritePipe {
    data: Mutex<VecDeque<u8>>
}

impl ReadWritePipe {
    pub const fn new() -> Self {
        Self {
            data: Mutex::new(VecDeque::new())
        }
    }

    pub fn take_output(&self) -> Vec<u8> {
        let mut lock = self.data.lock();
        let mut buf = Vec::with_capacity(lock.len());

        let (front, back) = lock.as_slices();
        buf.extend_from_slice(front);
        buf.extend_from_slice(back);
        lock.clear();

        buf
    }

    pub fn write(&self, data: &[u8]) {
        let mut lock = self.data.lock();
        lock.clear();
        lock.extend(data)
    }

    pub fn gc(&self) {
        self.data.lock().shrink_to_fit()
    }
}

type GetCtx = Box<dyn FnOnce() -> Weak<SharedCtx> + Send + Sync>;

#[derive(Clone)]
pub struct SharedCtxFile(Arc<Lazy<Weak<SharedCtx>, GetCtx>>);

impl SharedCtxFile {
    pub fn new() -> (Self, oneshot::Sender<Weak<SharedCtx>>) {
        let (rx, mut tx) = oneshot::channel();

        let cell = Lazy::new(
            Box::new(move || {
                tx.try_recv().expect("You need to send the context to the file before using")
            }) as GetCtx
        );

        (Self(Arc::new(cell)), rx)
    }

    fn get(&self) -> Arc<SharedCtx> {
        self.0.upgrade().expect("used outside of parent context")
    }
}

#[wiggle::async_trait]
impl WasiFile for SharedCtxFile {
    fn as_any(&self) -> &dyn Any {
        self
    }

    async fn get_filetype(&self) -> Result<FileType, Error> {
        Ok(FileType::Pipe)
    }
    async fn get_fdflags(&self) -> Result<FdFlags, Error> {
        Ok(FdFlags::APPEND)
    }
    async fn read_vectored<'a>(&self, bufs: &mut [io::IoSliceMut<'a>]) -> Result<u64, Error> {
        let n = self.get().stdin_pipe.data.lock().read_vectored(bufs)?;
        Ok(n.try_into()?)
    }

    async fn write_vectored<'a>(&self, bufs: &[io::IoSlice<'a>]) -> Result<u64, Error> {
        let n = self.get().stdout_pipe.data.lock().write_vectored(bufs)?;
        Ok(n.try_into()?)
    }
}