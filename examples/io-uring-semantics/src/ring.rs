use std::collections::VecDeque;
use std::{cell, rc::Rc, sync::Arc};

use io_uring::IoUring;
use tokio::io::unix::AsyncFd;
use tokio::sync::{Notify, Semaphore};

type TimeoutDeque = VecDeque<Option<Rc<dyn SubmitAllocation>>>;

/// Some memory allocation which we reference in `submit`.
///
/// The only relevant feature here is that the type be `Drop`.
pub trait SubmitAllocation {}

impl<T: 'static> SubmitAllocation for T {}

struct NotifyOnDrop(Arc<Notify>);

impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

pub struct Submit<'cell> {
    uring: cell::RefMut<'cell, IoUring>,
    n: usize,
    timeout: cell::RefMut<'cell, TimeoutDeque>,
}

pub(crate) struct TestCloseRing {
    /// A semaphore describing the available send space. The queue here defines the fairness
    /// criterion between different submission operations.
    fill: Semaphore,
    running: Semaphore,
    running_len: u32,
    io_uring: cell::RefCell<IoUring>,
    stable_timeouts: cell::RefCell<TimeoutDeque>,
    ready: Arc<Notify>,
    #[allow(dead_code)] // Only have this for aborting on drop.
    poller: NotifyOnDrop,
}

impl TestCloseRing {
    pub fn new() -> Result<Self, std::io::Error> {
        let mut io_uring = IoUring::new(0x100)?;
        let fill = io_uring.submission().capacity();

        let eventfd = uapi::eventfd(
            0,
            uapi::c::EFD_SEMAPHORE | uapi::c::EFD_NONBLOCK | uapi::c::EFD_CLOEXEC,
        )?;

        io_uring.submitter().register_eventfd(eventfd.raw())?;
        let eventfd = AsyncFd::new(eventfd)?;

        let ready = Arc::new(Notify::new());
        let notify = ready.clone();
        let abort = Arc::new(Notify::new());
        let abort_notify = abort.clone();

        tokio::spawn(async move {
            let mut eventfd = eventfd;
            let mut buf = [0; 8];

            while let Ok(mut ready) = {
                tokio::select!(
                    v = eventfd.readable_mut() => v,
                    _ = abort_notify.notified() => return,
                )
            } {
                match uapi::read(ready.get_inner_mut().raw(), &mut buf) {
                    Ok(_) | Err(uapi::Errno(uapi::c::EAGAIN)) => {}
                    _ => break,
                };

                ready.clear_ready();
                notify.notify_waiters();
                tokio::task::yield_now().await;
            }
        });

        Ok(TestCloseRing {
            fill: Semaphore::new(fill),
            running: Semaphore::new(fill),
            running_len: fill as u32,
            io_uring: io_uring.into(),
            ready,
            stable_timeouts: Default::default(),
            poller: NotifyOnDrop(abort),
        })
    }

    pub async fn collect(&self) {
        let fut = self.running.acquire_many(self.running_len);
        tokio::pin!(fut);

        loop {
            tokio::select! {
                _ = self.complete() => {},
                permit = &mut fut => {
                    drop(permit);
                    return;
                }
            }
        }
    }

    pub async fn non_empty_submission_and_then_sync(
        &self,
        n: u32,
    ) -> Result<Submit<'_>, std::io::Error> {
        let permit_run = loop {
            if let Ok(permit_run) = self.running.try_acquire_many(n) {
                break permit_run;
            };

            self.complete().await;
        };

        let _permit = self.fill.acquire_many(n).await.expect("Never closed");
        let n = usize::try_from(n).unwrap();

        loop {
            {
                let mut ring = self.io_uring.borrow_mut();
                let submission = ring.submission();
                let avail = submission.capacity() - submission.len();

                if avail >= n {
                    // This is enough to 'own' the non-empty submission queue until calling async task yields.
                    break;
                }
            }

            // We couldn't even submit enough, wait for some events to clear. Re-check for the next
            // time that the submission queue might become longer.
            self.complete().await;
        }

        permit_run.forget();
        Ok(Submit {
            uring: self.io_uring.borrow_mut(),
            n,
            timeout: self.stable_timeouts.borrow_mut(),
        })
    }

    /// Drive forward our submission towards the kernel, in the process of ensuring that the
    /// submissions are also completed at some point.
    fn try_submission(&self) {
        let mut ring = self.io_uring.borrow_mut();

        {
            // Make sure we do not block endlessly.
            let submission = ring.submission();
            if submission.is_empty() {
                return;
            }

            // Only do submission if we shouldn't poll more. And here we should process completions
            // anyways.
            if submission.taskrun() {
                return;
            }
        }

        if let Ok(n) = Self::submit(&mut ring, &self.stable_timeouts) {
            // Refill permits available for the fill queue.
            self.fill.add_permits(n);
        }
    }

    async fn complete(&self) {
        self.try_submission();
        let _ = self.ready.notified().await;

        let mut ring = self.io_uring.borrow_mut();
        // let mut notify = self.notify.borrow_mut();
        let completion = ring.completion();

        // FIXME: log this somewhere? Some potentially concurrent or even parallel insight into the
        // cycles here might be appreciated.
        let n = completion.count();
        self.running.add_permits(n);
    }

    fn submit(
        ring: &mut IoUring,
        stable_timeouts: &cell::RefCell<TimeoutDeque>,
    ) -> std::io::Result<usize> {
        let n = ring.submitter().submit()?;
        // After submissions, we no longer need to keep alive the referenced data. There is a
        // one-to-one correspondence between our queue for this and the submitted entries.
        stable_timeouts.borrow_mut().drain(..n).for_each(|_| ());
        Ok(n)
    }
}

impl Submit<'_> {
    /// Safety:
    ///
    /// Some entries contain pointers to data. The kernel requires them to be kept alive, until the
    /// submissions of these entries has been finished. The caller guarantees that all memory
    /// referenced in this way is either kept alive with the io-uring; OR kept alive through
    /// the reference-counted shared ownership passed in the second array.
    pub unsafe fn redeem_push<const N: usize>(
        mut self,
        entry: &[io_uring::squeue::Entry],
        to: [Rc<dyn SubmitAllocation>; N],
    ) {
        debug_assert_eq!(self.n, entry.len());
        assert!(N <= entry.len());

        let non_timeouts = entry.len() - N;
        self.uring.submission().push_multiple(entry).unwrap();

        self.timeout
            .extend(core::iter::repeat(None).take(non_timeouts));
        self.timeout.extend(to.map(Some));
    }
}
