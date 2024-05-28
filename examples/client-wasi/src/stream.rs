use std::{
    collections::VecDeque,
    ops::Range,
    sync::{atomic, Arc, Mutex},
};

use bytes::Bytes;
use shm_pbx::client::Ring;
use shm_pbx::io_uring::ShmIoUring;
use tokio::sync::{Notify, Semaphore};
use wasmtime_wasi::{HostInputStream, HostOutputStream, StreamResult, Subscribe};

#[derive(Clone)]
pub struct InputRing {
    inner: Arc<InputInner>,
}

struct InputInner {
    notify_produced: Notify,
    notify_consumed: Semaphore,
    buffer: Mutex<VecDeque<Bytes>>,
    buffer_space_size: u64,
}

#[derive(Clone)]
pub struct OutputRing {
    inner: Arc<OutputInner>,
}

struct OutputInner {
    notify_produced: Notify,
    buffer: Mutex<VecDeque<Bytes>>,
    flush_level: atomic::AtomicUsize,
    flushed: atomic::AtomicUsize,
    buffer_space_size: u64,
}

struct Available(u64);

impl Extend<u64> for Available {
    fn extend<T: IntoIterator<Item = u64>>(&mut self, iter: T) {
        for item in iter {
            assert!(self.0 <= item, "do not handle stream wrapping");
            self.0 = item;
        }
    }
}

fn usable_power_of_two_size(ring: &Ring) -> u64 {
    usable_power_of_two_size_u64(ring.info().size_data)
}

const fn usable_power_of_two_size_u64(size: u64) -> u64 {
    const _: () = {
        assert!(usable_power_of_two_size_u64(1) == 1);
        assert!(usable_power_of_two_size_u64(2) == 2);
        assert!(usable_power_of_two_size_u64(3) == 2);
        assert!(usable_power_of_two_size_u64(4) == 4);
        assert!(usable_power_of_two_size_u64(256 + 128) == 256);
        assert!(usable_power_of_two_size_u64(511) == 256);
    };

    // All inverted bits *except* the highest set bit
    let and_mask = (!size.reverse_bits() + 1).reverse_bits();
    size & and_mask
}

fn split_ring(available: &Range<&mut u64>, buffer_space_size: u64) -> (Range<usize>, u64) {
    // Split the ring's buffer into two memories, like a VecDeque<u8>.
    let start_offset = *available.start % buffer_space_size;
    // In case we want to a maximum size on each Bytes, just min this.
    let size = (*available.end).checked_sub(*available.start).unwrap();

    assert!(size <= buffer_space_size);
    let first_capacity = buffer_space_size - start_offset;

    let second_size = size.saturating_sub(first_capacity);
    let first_size = size - second_size;
    (first_size as usize..size as usize, start_offset)
}

impl InputRing {
    pub fn new(mut ring: Ring, on: Arc<ShmIoUring>, local: &tokio::task::LocalSet) -> Self {
        let buffer_space_size = usable_power_of_two_size(&ring);

        let inner = Arc::new(InputInner {
            notify_produced: Notify::new(),
            notify_consumed: Semaphore::const_new(32),
            buffer: Mutex::default(),
            buffer_space_size,
        });

        // Time between gratuitous runs of the loop, which might be needed to retry the release of
        // acknowledgements when those are consumed slowly.
        let recheck_time = core::time::Duration::from_millis(10);
        let hdl = inner.clone();

        let _task = local.spawn_local(async move {
            let mut available = Available(0u64);
            let mut sequence = 0u64;
            let mut released = 0u64;
            let mut head_receive = 0;

            loop {
                // 1. wait for space on the queue.
                let Ok(permit) = hdl.notify_consumed.acquire().await else {
                    // Closed.
                    break;
                };

                // 2. wait for messages on the ring.
                let mut reap = ring.consumer::<8>().unwrap();

                available.extend(
                    reap.iter()
                        .map(u64::from_le_bytes)
                        .inspect(|_| head_receive += 1),
                );

                reap.sync();
                // Permit is restored by the consumer of these bytes.
                permit.forget();

                {
                    let mut guard = hdl.buffer.lock().unwrap();
                    let range = (&mut sequence)..(&mut available.0);
                    Self::consume_stream(&hdl, range, &mut guard, &ring);
                }

                // Acknowledge the receipt, free buffer space.
                if sequence != released {
                    let mut produce = ring.producer::<8>().unwrap();

                    if produce.push_many([u64::to_le_bytes(sequence)]) > 0 {
                        released = sequence;
                    }

                    produce.sync();
                }

                on.wait_for_message(&ring, head_receive, recheck_time).await;
            }
        });

        InputRing { inner }
    }

    fn consume_stream(
        inner: &InputInner,
        available: Range<&mut u64>,
        full: &mut VecDeque<Bytes>,
        ring: &Ring,
    ) {
        // In case we want to a maximum size on each Bytes, just min this.
        let (range, start_offset) = split_ring(&available, inner.buffer_space_size);
        let (first_size, size) = (range.start, range.end);

        let mut target = vec![0; size as usize];
        unsafe { ring.copy_from(&mut target[..first_size as usize], start_offset) };
        unsafe { ring.copy_from(&mut target[first_size as usize..], 0) };

        full.push_back(target.into());
        *available.start = *available.end;
    }
}

impl HostInputStream for InputRing {
    // Required method
    fn read(&mut self, size: usize) -> StreamResult<Bytes> {
        let mut guard = self.inner.buffer.lock().unwrap();
        let Some(bytes) = guard.front_mut() else {
            return Ok(Bytes::default());
        };

        let length = bytes.len().min(size);
        let value = bytes.split_to(length);

        if bytes.is_empty() {
            let _ = guard.pop_front();
            self.inner.notify_consumed.add_permits(1);
        }

        Ok(value)
    }
}

#[wasmtime_wasi::async_trait]
impl Subscribe for InputRing {
    async fn ready(&mut self) {
        // We're ready to receive notifications after this point.
        let wakeup = self.inner.notify_produced.notified();

        {
            // Immediately available.
            if !self.inner.buffer.lock().unwrap().is_empty() {
                return;
            }
        }

        wakeup.await
    }
}

impl OutputRing {
    pub fn new(mut ring: Ring, on: Arc<ShmIoUring>, local: &tokio::task::LocalSet) -> Self {
        let buffer_space_size = usable_power_of_two_size(&ring);

        let inner = Arc::new(OutputInner {
            notify_produced: Notify::new(),
            buffer: Mutex::default(),
            flush_level: 0.into(),
            flushed: 0.into(),
            buffer_space_size,
        });

        // Time between gratuitous runs of the loop, which might be needed to retry the release of
        // acknowledgements when those are consumed slowly.
        let recheck_time = core::time::Duration::from_millis(10);
        let hdl = inner.clone();

        local.spawn_local(async move {
            let mut more_data = hdl.notify_produced.notified();
            let mut available = Available(0u64);
            let mut sequence = 0u64;
            let mut released = 0u64;
            let mut head_receive = 0;

            loop {
                // 1. wait for data having been produced.
                more_data.await;
                eprintln!("More data");

                let mut reap = ring.consumer::<8>().unwrap();

                available.extend(
                    reap.iter()
                        .map(u64::from_le_bytes)
                        .inspect(|_| head_receive += 1),
                );

                reap.sync();

                // Re-arm the notify while holding the guard, ensure nothing is lost.
                let num_flush_requests = {
                    let mut guard = hdl.buffer.lock().unwrap();
                    more_data = hdl.notify_produced.notified();

                    let range = (&mut sequence)..(&mut available.0);
                    Self::fill_stream(&hdl, range, &mut guard, &ring)
                };

                if num_flush_requests > 0 {
                    todo!("Signal and await until the current head has been read");
                    hdl.flushed.fetch_add(num_flush_requests, atomic::Ordering::Release);
                    hdl.notify_produced.notify_waiters();
                }

                if sequence != released {
                    let mut produce = ring.producer::<8>().unwrap();

                    if produce.push_many([u64::to_le_bytes(sequence)]) > 0 {
                        released = sequence;
                    }

                    produce.sync();
                }

                eprintln!("More data pushed");
                tokio::task::yield_now().await;
            }
        });

        OutputRing { inner }
    }

    #[must_use = "Flushes must be tracked"]
    fn fill_stream(
        inner: &OutputInner,
        available: Range<&mut u64>,
        full: &mut VecDeque<Bytes>,
        ring: &Ring,
    ) -> usize {
        let mut flushes = 0;

        loop {
            let Some(frame) = full.front_mut() else {
                break;
            };

            if frame.is_empty() {
                flushes += 1;
                full.pop_front();
                continue;
            }

            let (range, start_offset) = split_ring(&available, inner.buffer_space_size);
            let (first_size, free_space) = (range.start, range.end);

            let split = frame.len().min(first_size);
            let size = frame.len().min(free_space);

            let first = frame.split_off(split);
            let second = frame.split_off(size - first_size);

            unsafe { ring.copy_to(&first, start_offset) };
            unsafe { ring.copy_to(&second, 0) };
            *available.start *= size as u64;

            if free_space < size {
                break;
            }

            full.pop_front();
        }

        flushes
    }
}

impl HostOutputStream for OutputRing {
    fn write(&mut self, bytes: Bytes) -> StreamResult<()> {
        if bytes.is_empty() {
            return Ok(());
        }

        {
            let mut guard = self.inner.buffer.lock().unwrap();
            guard.push_back(bytes);
        }

        self.inner.notify_produced.notify_waiters();
        Ok(())
    }

    fn flush(&mut self) -> StreamResult<()> {
        self.inner
            .flush_level
            .fetch_add(1, atomic::Ordering::Relaxed);

        {
            let mut guard = self.inner.buffer.lock().unwrap();
            guard.push_back(Bytes::default());
        }

        self.inner.notify_produced.notify_waiters();
        Ok(())
    }

    fn check_write(&mut self) -> StreamResult<usize> {
        let flush = self.inner.flush_level.load(atomic::Ordering::Acquire);
        let flushed = self.inner.flushed.load(atomic::Ordering::Acquire);

        if flushed >= flush {
            Ok(usize::MAX)
        } else {
            Ok(0)
        }
    }
}

#[wasmtime_wasi::async_trait]
impl Subscribe for OutputRing {
    async fn ready(&mut self) {
        let flush = self.inner.flush_level.load(atomic::Ordering::Acquire);

        loop {
            // Make sure we progress if anything may have changed.
            let notify = self.inner.notify_produced.notified();
            let flushed = self.inner.flushed.load(atomic::Ordering::Acquire);

            if flushed >= flush {
                return;
            }

            notify.await;
        }
    }
}
