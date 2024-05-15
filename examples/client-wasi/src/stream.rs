use std::sync::Arc;

use bytes::Bytes;
use shm_pbx::client::Ring;
use shm_pbx::io_uring::ShmIoUring;
use wasmtime_wasi::{HostInputStream, HostOutputStream, Subscribe, StreamResult};

#[derive(Clone)]
struct InputRing {
    inner: Arc<Ring>,
}

#[derive(Clone)]
struct OutputRing {
    inner: Arc<Ring>,
}

#[wasmtime_wasi::async_trait]
impl Subscribe for InputRing {
    async fn ready(&mut self) {
        todo!()
    }
}

#[wasmtime_wasi::async_trait]
impl Subscribe for OutputRing {
    async fn ready(&mut self) {
        todo!()
    }
}

impl HostInputStream for InputRing {
    // Required method
    fn read(&mut self, _size: usize) -> StreamResult<Bytes> {
        todo!()
    }
}

impl HostOutputStream for OutputRing {
    fn write(&mut self, bytes: Bytes) -> StreamResult<()> {
        todo!()
    }

    fn flush(&mut self) -> StreamResult<()> {
        todo!()
    }

    fn check_write(&mut self) -> StreamResult<usize> {
        todo!()
    }
}
