use std::any::Any;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Arc;
use parking_lot::Mutex;
use wasi_common::file::{FdFlags, FileType};
use wasi_common::{Error, WasiFile};

#[derive(Debug, Clone)]
pub struct ReadWritePipe {
    data: Arc<Mutex<VecDeque<u8>>>
}

impl ReadWritePipe {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: Arc::new(Mutex::new(VecDeque::with_capacity(capacity)))
        }
    }

    pub fn read(&self, amount: usize) -> anyhow::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(amount);
        (&mut *self.data.lock()).take(amount as u64).read_to_end(&mut buf).unwrap();
        anyhow::ensure!(
            buf.len() == amount,
            "invalid amount read {amount}, but buffer only contained {}", buf.len()
        );
        Ok(buf)
    }

    pub fn write(&self, data: &[u8]) {
        self.data.lock().extend(data)
    }
}

#[wiggle::async_trait]
impl WasiFile for ReadWritePipe {
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
        let n = self.data.lock().read_vectored(bufs)?;
        Ok(n.try_into()?)
    }

    async fn write_vectored<'a>(&self, bufs: &[io::IoSlice<'a>]) -> Result<u64, Error> {
        let n = self.data.lock().write_vectored(bufs)?;
        Ok(n.try_into()?)
    }
}