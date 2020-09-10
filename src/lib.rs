//! An IPC task queue similar to io-uring.
//!
//! Use with any language capable of mapping anonymous files and accessing them as bytes.
//!
//! ## Joining
//!
//! 1. Inspect the monitor head to determine a join method that is implemented.
//! 1.1. Might inspect the member count to see if joining is probable at all.
//!
//! 2. Joining.
//!    The problem here is to distribute client identities. This process must not race between
//!    different clients (potential members). We may want some mediation as well. At the same time
//!    we need to notify the controller of our initial presence (which must setup any watch dog if
//!    we consider sudden exits). All of this must happen efficiently such that no spin lock is
//!    necessary. Basically we need an alternative to interrupts/syscalls here.
//! 2.1 The first step is to grab one client spot. This is done by atomically exchanging a client
//!   entry for your own process ID. This allows the server to detect if the process is gone. This
//!   has false positives if PIDs are reused but no false negatives so that no active state is
//!   destroyed. The next step depends on the synchronization method.
//!
//! 2.2. Joining via a pipe.
//! 2.2.1. Write the member ID as a Network Endian integer into the indicated pipe.
//!
//! 2.2. Joining via a futex.
//! 2.2.2. Try to update the `futex` of the open page from `0` to your member ID.
//! 2.2.3. Wait for the controller to notice. It will unlock the futex to `0` when it does. On
//!   failure, another process interfered, retry.
//!
//! ## Thread (/threat) model
//!
//! This library will NOT protect against malicious writer that have access to the shared memory
//! file. It fundamentally can not, they may simply map the file on their own and go ham. Instead
//! it tries to mediate conflicts and assign responsibilities between cooperating threads.
//!
//! Nevertheless, one must consider misbehaving threads such as a communication partner stalled on
//! a blocking IO operation or an unwanted process crash. Both have a similar syndrome, a command
//! queue never progresses or an active state is not cleared, but different resolutions. In the
//! first case any active destruction must carefully consider which invariants the held thread can
//! assume and mustn't violate them. Otherwise it could, for example, at a later point in time
//! resume and unwillingly interfere with another thread that took its place. The second case can
//! be resolved more forcefully but may be different (harder?) to detect.
pub mod control;
#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod os_impl;
#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod os_impl;

use core::{cell, mem, ptr, slice};
use core::convert::TryFrom;
use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};

use shared_memory::Shmem;

pub const MEMBER_LIMIT: usize = 256;

/// The monitor head is _only_ written by the controller.
#[repr(C)]
pub struct MonitorHead {
    magic: AtomicU32,
    /// Static version of this header structure.
    version: AtomicU32,
    /// A bit-mask of available methods for opening a new handle.
    /// `0x00000001`: wait by reading from a named pipe
    /// `0x00000002`: futex based waiting using the futex in the head (?? WIP details)
    /// `0x00000004`: wait using an semaphore (?? WIP details)
    open_with: AtomicU32,
    /// File-relative address of the page for new process to join.
    open_page: AtomicU64,
    /// File-relative address of the first page used by queue heads.
    head_start: AtomicU64,
    /// File-relative address of the first page used by queues.
    queue_start: AtomicU64,
    /// File-relative address of the first page used for shared memory of queues.
    heap_start: AtomicU64,
    /// The number of current members.
    members: AtomicU32,
    /// The maximal number of members.
    max_members: AtomicU32,
    /// The start address of the pipe's name if any.
    pipe_name: AtomicU64,
    /// The start address of the member list. It has `max_members` entries.
    member_list: AtomicU64,
}

/// The page with which new clients communicate with the controller.
#[repr(C)]
pub struct MonitorOpen {
    /// A shared register where a process can write its address.
    /// Must first hold the futex.
    tid_futex: u64,
    /// A shared register where a process can write its address.
    /// Must first hold the semaphore.
    tid_semaphore: u64,
    /// Futex to wait on for global control operations such as join.
    futex: AtomicI32,
    _align_futex: u32, // Filler.
}

/// A queue between two threads.
#[repr(C)]
pub struct QueueHead {
    /// The struct controlled by side A.
    half_a: QueueHalf,
    /// The struct controlled by side B.
    half_b: QueueHalf,
}

/// A reader from a queue.
pub struct ReadHalf<'queue> {
    inner: &'queue QueueHalf,
    buffer: &'queue cell::UnsafeCell<[u8]>,
}

/// A reader from a queue.
pub struct WriteHalf<'queue> {
    inner: &'queue QueueHalf,
    buffer: &'queue cell::UnsafeCell<[u8]>,
}

#[repr(C)]
pub struct QueueHalf {
    /// First futex that the threads may use.
    futex_a: AtomicI32,
    _reserved: u32,
    /// The write head.
    write_head: AtomicU32,
    /// The read head.
    read_head: AtomicU32,
    /// Private use data for this side.
    user_data: cell::UnsafeCell<u64>,
}

#[derive(Clone, Copy)]
#[repr(u32)]
pub enum JoinMethod {
    Pipe = 0x00000001,
    Futex = 0x00000002,
    Semaphore = 0x00000004,
}

pub struct OpenOptions {
    num_rings: u32,
    max_members: u32,
}

/// Valid pointers to a shm-ring head.
#[derive(Clone, Copy)]
struct ShmHeads {
    monitor: ptr::NonNull<MonitorHead>,
    open: ptr::NonNull<MonitorOpen>,
}

pub struct ShmController {
    shared: Shmem,
    heads: ShmHeads,
}

/// A client looking to join the ring.
/// Note that you can join multiple times for independent control connections.
pub struct ShmClient {
    shared: Shmem,
    heads: ShmHeads,
    join_with: JoinMethod,
}

/// A single queue in the ring.
/// IMPORTANT: This is an owning structure of this half of the queue, which is a safety
/// invariant. You must not call methods other than `reverse` on the half you do not own. Otherwise
/// the read or write buffer might be out-of-sync of the synchronized range of bytes in the
/// respective queues.
struct ShmQueues {
    private_head: ptr::NonNull<QueueHead>,
    private_queues: ptr::NonNull<u64>,
    private_mem: ptr::NonNull<u8>,
    is_a_side: bool,
}

/// An active connection to another client (or the controller).
pub struct ShmRing {
    shared: Shmem,
    heads: ShmHeads,
    queues: ShmQueues,
}

/// An active connection to the controller.
pub struct ShmControllerRing {
    pub ring: ShmRing,
    /// The client id owned by this ring.
    this: u32,
    /// The pid we registered..
    pid: u64,
    _private: (),
}

/// An active connection to another client.
pub struct ShmClientRing {
    pub ring: ShmRing,
    _private: (),
}

#[derive(Debug)]
pub struct ConnectError;
#[derive(Debug)]
pub struct JoinError;
#[derive(Debug)]
pub struct OpenError;
#[derive(Debug)]
pub struct SendError;
#[derive(Debug)]
pub struct RecvError;

impl OpenOptions {
    const PAGE_SIZE: usize = 4096;
    // TODO: do we want a useful magic before release?
    const MAGIC: u32 = 0x9d14_c252;

    pub fn new() -> Self {
        OpenOptions {
            num_rings: 64,
            max_members: 32,
        }
    }

    pub fn open(self, path: &std::path::Path) -> Result<ShmClient, shared_memory::ShmemError> {
        let shared = shared_memory::ShmemConf::new()
            .flink(path)
            .open()?;
        assert!(shared.len() >= mem::size_of::<MonitorHead>());
        let head = ptr::NonNull::new(shared.as_ptr()).unwrap();
        let heads = unsafe {
            ShmHeads::from_monitor(head.cast())
        }.map_err(|_| shared_memory::ShmemError::MapSizeZero)?;

        if heads.monitor().magic.load(Ordering::Acquire) != OpenOptions::MAGIC {
            return Err(shared_memory::ShmemError::MapSizeZero);
        }

        Ok(ShmClient {
            shared,
            heads,
            join_with: JoinMethod::Pipe,
        })
    }

    pub fn create(self, path: &std::path::Path) -> Result<ShmController, shared_memory::ShmemError> {
        assert!(self.max_members <= self.num_rings, "At least one ring per potential member is required");
        let pages = self.num_rings as usize * 5 + 3;
        let size = pages * Self::PAGE_SIZE;
        let shared = shared_memory::ShmemConf::new()
            .flink(path)
            // Three pages for the controller: head, open, free
            // Five pages per ring (one head, 2 for commands, 2 buffers)
            .size(size)
            .create()?;
        assert!(shared.len() >= size);
        let (monitor, open);
        unsafe {
            monitor = ptr::NonNull::new(shared.as_ptr()).unwrap();
            open = ptr::NonNull::new(monitor.as_ptr().add(Self::PAGE_SIZE)).unwrap();
        }

        let controller = ShmController {
            shared,
            heads: ShmHeads {
                monitor: monitor.cast(),
                open: open.cast(),
            }
        };

        // Init sequence: write static information and lastly write the open_with information
        let monitor = controller.monitor();
        monitor.magic.store(OpenOptions::MAGIC, Ordering::Release);
        monitor.version.store(1, Ordering::Release);
        monitor.open_page.store(Self::PAGE_SIZE as u64, Ordering::Release);
        let first_head = 3;
        monitor.head_start.store(first_head * Self::PAGE_SIZE as u64, Ordering::Release);
        let first_queue = first_head + u64::from(self.num_rings);
        monitor.queue_start.store(first_queue * Self::PAGE_SIZE as u64, Ordering::Release);
        let first_heap = first_queue + 2 * u64::from(self.num_rings);
        monitor.heap_start.store(first_heap * Self::PAGE_SIZE as u64, Ordering::Release);
        monitor.members.store(0, Ordering::Release);
        monitor.max_members.store(self.max_members, Ordering::Release);
        monitor.pipe_name.store(0, Ordering::Release);
        // For now, starts directly behind head.
        let member_list_start = u64::try_from(mem::size_of::<MonitorHead>()).unwrap();
        monitor.member_list.store(member_list_start, Ordering::Release);
        assert_eq!(controller.member_list().len(), self.max_members as usize);
        for member in controller.member_list() {
            member.store(0, Ordering::Release);
        }

        controller.open().futex.store(-1, Ordering::Release);

        // Enable futex based join.
        // TODO: can't do that on all systems.
        monitor.open_with.store(0x2, Ordering::Release);

        Ok(controller)
    }
}

impl ShmController {
    pub fn path(&self) -> &std::path::Path {
        self.shared.get_flink_path().unwrap()
    }

    pub fn raw_join_methods(&self) -> u32 {
        self.monitor().open_with.load(Ordering::Relaxed)
    }

    pub fn members(&self) -> (u32, u32) {
        self.monitor().members()
    }

    /// Handle a round of messages.
    /// Will never block but might take a while?
    pub fn enter(&mut self) {
        self.handle_new_client();
        self.handle_client_queues();
    }

    fn handle_new_client(&self) {
        os_impl::join_futex(self, |request| {
            self.heads.queues_controller(request).reset();
            self.monitor().members.fetch_add(1, Ordering::Relaxed);
        });
    }

    fn handle_client_queues(&self) {
        let clients = self.monitor().max_members.load(Ordering::Relaxed);
        for i in 0..clients {
            let queue = self.heads.queues_controller(i);
            control::ControlMessage::execute(self, queue);
        }
    }

    pub(crate) fn monitor(&self) -> &MonitorHead {
        self.heads.monitor()
    }

    pub(crate) fn open(&self) -> &MonitorOpen {
        self.heads.open()
    }

    pub(crate) fn member_list(&self) -> &[AtomicU64] {
        self.heads.member_list()
    }
}

impl ShmClient {
    pub fn raw_join_methods(&self) -> u32 {
        self.monitor().open_with.load(Ordering::Relaxed)
    }

    pub fn members(&self) -> (u32, u32) {
        self.monitor().members()
    }

    pub fn join_with(mut self, method: JoinMethod) -> Self {
        self.join_with = method;
        self
    }

    pub fn connect(self) -> Result<ShmControllerRing, ConnectError> {
        let allowed = self.monitor().open_with.load(Ordering::Acquire);

        if self.join_with.as_u32() & allowed == 0 {
            return Err(ConnectError);
        }

        let pid = os_impl::own_pid();
        let want = self.find_open_client(pid)?;
        println!("{}@{}", pid, want);

        let queue = match self.join_with {
            JoinMethod::Pipe | JoinMethod::Semaphore => {
                self.nicely_rescind_client(want, pid);
                return Err(ConnectError);
            },
            JoinMethod::Futex => os_impl::futex(&self, want),
        };

        let this = queue?;

        let ring = ShmRing {
            queues: self.heads.queues_controller(this).reverse(),
            shared: self.shared,
            heads: self.heads,
        };

        Ok(ShmControllerRing {
            ring,
            this,
            pid,
            _private: (),
        })
    }

    pub fn try_clone(&self) -> Result<Self, shared_memory::ShmemError> {
        // No Clone impl for Shmem so we do it manually.
        let shared = shared_memory::ShmemConf::new()
            .flink(self.shared.get_flink_path().unwrap())
            .open()?;
        assert_eq!(self.shared.as_ptr(), shared.as_ptr());
        Ok(ShmClient {
            shared,
            ..*self
        })
    }

    fn find_open_client(&self, pid: u64) -> Result<u32, ConnectError> {
        let clients = self.member_list();

        loop {
            for (idx, c) in clients.iter().enumerate() {
                match c.compare_exchange(0, pid, Ordering::AcqRel, Ordering::Relaxed) {
                    Ok(_) => return Ok(idx as u32),
                    Err(_) => {},
                }
            }

            return Err(ConnectError);
        }
    }

    fn nicely_rescind_client(&self, this: u32, pid: u64) {
        let clients = self.member_list();
        // We should be the only updating this right now. So ignore errors..
        // This would indicate someone else already has grabbed control. Sure.
        let _ = clients[this as usize].compare_exchange(pid, 0, Ordering::Relaxed, Ordering::Relaxed);
    }

    pub(crate) fn monitor(&self) -> &MonitorHead {
        self.heads.monitor()
    }

    pub(crate) fn open(&self) -> &MonitorOpen {
        self.heads.open()
    }

    pub(crate) fn member_list(&self) -> &[AtomicU64] {
        self.heads.member_list()
    }
}

impl JoinMethod {
    /// Simple shim to avoid mistyping the integral type.
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

impl ShmRing {
    pub fn send(&mut self, data: &[u8]) -> Result<(), SendError> {
        let len = u32::try_from(data.len()).map_err(|_| SendError)?;
        let mut writer = self.queues.half_to_write_to();

        let write = writer.prepare_write(len).ok_or(SendError)?;
        let (lhs, rhs) = data.split_at(write.unwrapped.len());
        write.unwrapped.copy_from_slice(lhs);
        write.wrapped.copy_from_slice(rhs);
        write.commit();

        Ok(())
    }

    pub fn recv<T>(
        &mut self,
        data: &mut [u8],
        pre_commit: impl FnOnce(&mut [u8], &ReadHalf) -> T
    ) -> Result<T, RecvError> {
        let len = u32::try_from(data.len()).map_err(|_| RecvError)?;
        let mut reader = self.queues.half_to_read_from();

        let read = reader.available(len).ok_or(RecvError)?;
        let (lhs, rhs) = data.split_at_mut(read.unwrapped.len());
        lhs.copy_from_slice(read.unwrapped);
        rhs.copy_from_slice(read.wrapped);
        let result = pre_commit(data, read.commit);
        read.commit();

        Ok(result)
    }
}

impl ShmControllerRing {
    pub fn request(&mut self, msg: impl Into<control::ControlMessage>) -> Result<(), SendError> {
        let msg = msg.into().op.to_be_bytes();
        self.ring.send(&msg)
    }

    pub fn response<T>(&mut self, handler: impl FnOnce(control::ControlResponse) -> T) -> Result<T, RecvError> {
        let mut op = [0; 8];
        self.ring.recv(&mut op, |filled, ring| {
            let op = <&mut [u8; 8]>::try_from(filled).unwrap();
            let resp = control::ControlResponse::new(ring, *op);
            handler(resp)
        })
    }
}

impl ShmHeads {
    pub(crate) unsafe fn from_monitor(ptr: ptr::NonNull<MonitorHead>) -> Result<Self, OpenError> {
        let monitor = &*ptr.as_ptr();
        if monitor.open_with.load(Ordering::Acquire) == 0 {
            // Oops, not yet initialized.
            return Err(OpenError);
        }
        let open_offset = monitor.open_page.load(Ordering::Acquire);
        let open_offset = usize::try_from(open_offset).map_err(|_| OpenError)?;
        let open_ptr = ptr::NonNull::new((ptr.as_ptr() as *mut u8).add(open_offset)).unwrap();
        Ok(ShmHeads {
            monitor: ptr,
            open: open_ptr.cast(),
        })
    }
    pub(crate) fn monitor(&self) -> &MonitorHead {
        assert!(mem::size_of::<MonitorHead>() <= OpenOptions::PAGE_SIZE);
        unsafe { &*(self.monitor.as_ptr() as *const MonitorHead) }
    }

    pub(crate) fn open(&self) -> &MonitorOpen {
        assert!(mem::size_of::<MonitorOpen>() <= OpenOptions::PAGE_SIZE);
        unsafe { &*(self.monitor.as_ptr() as *const MonitorOpen) }
    }

    pub(crate) fn member_list(&self) -> &[AtomicU64] {
        let space = OpenOptions::PAGE_SIZE - mem::size_of::<MonitorHead>();
        let max_members = self.monitor().max_members.load(Ordering::Relaxed) as usize;
        assert!(space / mem::size_of::<AtomicU64>() >= max_members,
            "Not enough space for inline member list. Should have caught this in constructor");
        let base = self.monitor.as_ptr();
        unsafe {
            // SAFETY: surely short enough, and enough space for max_members as validated above.
            let memlist = (base as *const MonitorHead).add(1) as *const AtomicU64;
            slice::from_raw_parts(memlist, max_members)
        }
    }

    fn head_start(&self, this: u32) -> ptr::NonNull<QueueHead> {
        let base = self.monitor.as_ptr() as *mut u8;

        let start = self.monitor().head_start.load(Ordering::Relaxed) as usize;
        let offset = (this as usize) * OpenOptions::PAGE_SIZE;

        unsafe {
            ptr::NonNull::new(base.add(start).add(offset) as *mut QueueHead).unwrap()
        }
    }

    fn queue_start(&self, this: u32) -> ptr::NonNull<u64> {
        let base = self.monitor.as_ptr() as *mut u8;

        let start = self.monitor().queue_start.load(Ordering::Relaxed) as usize;
        let offset = 2 * (this as usize) * OpenOptions::PAGE_SIZE;

        unsafe {
            ptr::NonNull::new(base.add(start).add(offset) as *mut u64).unwrap()
        }
    }

    fn heap_start(&self, this: u32) -> ptr::NonNull<u8> {
        let base = self.monitor.as_ptr() as *mut u8;

        let start = self.monitor().head_start.load(Ordering::Relaxed) as usize;
        let offset = 2 * (this as usize) * OpenOptions::PAGE_SIZE;

        unsafe {
            ptr::NonNull::new(base.add(start).add(offset)).unwrap()
        }
    }

    /// FIXME: should we make this unsafe? Returned value assumes to own that side of the queue.
    fn queues_controller(&self, this: u32) -> ShmQueues {
        let (private_head, private_queues, private_mem);
        // FIXME: validated pointers? We trust the server but it's a precaution against
        // alternative implementations that are wrong..
        private_head = self.head_start(this);
        private_queues = self.queue_start(this);
        private_mem = self.heap_start(this);

        ShmQueues {
            private_head,
            private_queues,
            private_mem,
            is_a_side: true,
        }
    }
}

impl MonitorHead {
    pub fn members(&self) -> (u32, u32) {
        let now = self.members.load(Ordering::Acquire);
        let max = self.max_members.load(Ordering::Acquire);
        (now, max)
    }
}

impl ShmQueues {
    fn reverse(self) -> Self {
        ShmQueues {
            is_a_side: !self.is_a_side,
            ..self
        }
    }

    fn head(&self) -> &QueueHead {
        unsafe { &*self.private_head.as_ptr() }
    }

    /// Reset the queue. Must guarantee that we have a unique reference to it!
    fn reset(&self) {
        let queue = self.head();
        queue.half_a.reset();
        queue.half_b.reset();
    }

    fn half_to_read_from(&mut self) -> ReadHalf<'_> {
        let queue = self.head();
        let (inner, offset) = if self.is_a_side {
            (&queue.half_b, OpenOptions::PAGE_SIZE)
        } else {
            (&queue.half_a, 0)
        };
        let buffer = unsafe {
            let addr = (self.private_queues.as_ptr() as *mut u8).add(offset);
            let buffer = ptr::slice_from_raw_parts(addr, OpenOptions::PAGE_SIZE);
            &*(buffer as *const cell::UnsafeCell<[u8]>)
        };
        ReadHalf {
            inner,
            buffer,
        }
    }

    fn half_to_write_to(&mut self) -> WriteHalf<'_> {
        let queue = unsafe { &*self.private_head.as_ptr() };
        let (inner, offset) = if self.is_a_side {
            (&queue.half_a, 0)
        } else {
            (&queue.half_b, OpenOptions::PAGE_SIZE)
        };
        let buffer = unsafe {
            let addr = (self.private_queues.as_ptr() as *mut u8).add(offset);
            let buffer = ptr::slice_from_raw_parts(addr, OpenOptions::PAGE_SIZE);
            &*(buffer as *const cell::UnsafeCell<[u8]>)
        };
        WriteHalf {
            inner,
            buffer,
        }
    }
}

impl QueueHalf {
    fn reset(&self) {
        self.futex_a.store(0, Ordering::Relaxed);
        self.read_head.store(0, Ordering::Relaxed);
        self.write_head.store(0, Ordering::Relaxed);
        let atomic_user = unsafe { &*(self.user_data.get() as *const AtomicU64) };
        atomic_user.store(0, Ordering::Relaxed);
    }
}

// FIXME: perfectly honest, those references to the buffer aren't right. The contents may change
// and we would need freeze to avoid it.
struct PreparedRead<'buffer> {
    /// Access to the underlying reader.
    commit: &'buffer ReadHalf<'buffer>,
    unwrapped: &'buffer [u8],
    wrapped: &'buffer [u8],
    read_base: u32,
    read_end: u32,
}

struct PreparedWrite<'buffer> {
    /// Access to the underlying writer.
    commit: &'buffer WriteHalf<'buffer>,
    unwrapped: &'buffer mut [u8],
    wrapped: &'buffer mut [u8],
    write_base: u32,
    write_end: u32,
}

impl ReadHalf<'_> {
    /// Return (reader, ready) if it is at least count bytes.
    fn acquire_ready(&self, count: u32) -> Option<(u32, u32)> {
        // On success we must have read with Acquire to sync the buffer contents.
        let writer = self.inner.write_head.load(Ordering::Acquire);
        // But not this, this is published by us..
        let reader = self.inner.read_head.load(Ordering::Relaxed);
        let modulo = OpenOptions::PAGE_SIZE as u32;

        let ready = ((writer + modulo) - reader) % modulo;
        if ready < count {
            None
        } else {
            Some((reader, ready))
        }
    }

    fn available(&mut self, count: u32) -> Option<PreparedRead<'_>> {
        let (reader, _) = self.acquire_ready(count)?;
        let modulo = OpenOptions::PAGE_SIZE as u32;

        let norm_half = (reader + count).min(modulo);
        let wrap_half = (reader + count).max(modulo) - modulo;

        let buffer = self.buffer.get() as *const u8;

        // SAFETY: synchronized as required. The other thread has release this.
        let (first, wrapped) = unsafe {
            // By taking `&mut self` we are indeed the only referent. In particular, the other side
            // should only write again after we have ACKed this part but we do not allow committing
            // that while the return value lives.
            // FIXME: taking borrow on bytes, should probably only take a borrow on the atomic slice.
            // Do we trust the other side unsafely?
            let norm = &*slice::from_raw_parts(buffer.add(reader as usize), norm_half as usize);
            let wrapping = &*slice::from_raw_parts(buffer, wrap_half as usize);
            (norm, wrapping)
        };
        Some(PreparedRead {
            commit: &*self,
            read_base: reader,
            read_end: (reader + count) % modulo,
            unwrapped: first,
            wrapped,
        })
    }
}

impl PreparedRead<'_> {
    /// Atomically remove one value.
    fn controller_u64(&self) -> u64 {
        let bh = self.unwrapped;
        let lh = self.wrapped;

        // We only ever dequeue 64 at once..
        assert!(bh.is_empty() || lh.is_empty());
        assert_eq!(bh.len() + lh.len(), 8);
        let bh = <[u8; 8]>::try_from(bh);
        let lh = <[u8; 8]>::try_from(lh);
        let val = bh.or(lh).expect("logic bug");
        u64::from_be_bytes(val)
    }

    fn commit(self) {
        if cfg!(debug_assertions) {
            let must_succeed = self.commit.inner.read_head.compare_exchange(
                self.read_base, self.read_end, Ordering::Release, Ordering::Relaxed);
            must_succeed.expect("Another thread accessed supposedly owned read head");
        } else {
            self.commit.inner.read_head.store(self.read_end, Ordering::Release);
        }
    }
}

impl WriteHalf<'_> {
    /// Return (reader, ready) if it is at least count bytes.
    fn acquire_ready(&self, count: u32) -> Option<(u32, u32)> {
        // The write head is owned by us, don't care a lot.
        let writer = self.inner.write_head.load(Ordering::Relaxed);
        // But not this, we must not touch entries that are still being read.
        let reader = self.inner.read_head.load(Ordering::Acquire);
        let modulo = OpenOptions::PAGE_SIZE as u32;

        // Why -1? Two reasons: Firstly we must never bump the write pointer into the read pointer.
        // That is an empty buffer. So conveniently we calculate the last allowed address instead
        // by subtracting one (note after adding page size so no overflows) and then do a
        // comparison against that.
        let empty = ((reader + modulo - 1) - writer) % modulo;
        if empty < count { // Strict, new end must be smaller than reader.
            None
        } else {
            Some((writer, empty + 1))
        }
    }

    fn prepare_write(&mut self, count: u32) -> Option<PreparedWrite<'_>> {
        let (writer, _) = self.acquire_ready(count)?;
        let modulo = OpenOptions::PAGE_SIZE as u32;

        let norm_half = (writer + count).min(modulo);
        let wrap_half = (writer + count).max(modulo) - modulo;

        let buffer = self.buffer.get() as *mut u8;

        // SAFETY: synchronized as required. The other thread has release this.
        let (first, wrapped) = unsafe {
            // By taking `&mut self` we are indeed the only referent. In particular, the other side
            // should only read the data after we have ACKed this part but we do not allow committing
            // that while the return value lives.
            // FIXME: taking mutable borrow, should probably only take a borrow on the atomic slice.
            // Do we trust the other side unsafely?
            let norm = &mut *slice::from_raw_parts_mut(buffer.add(writer as usize), norm_half as usize);
            let wrapping = &mut *slice::from_raw_parts_mut(buffer, wrap_half as usize);
            (norm, wrapping)
        };
        Some(PreparedWrite {
            commit: &*self,
            write_base: writer,
            write_end: (writer + count) % modulo,
            unwrapped: first,
            wrapped,
        })
    }
}

impl PreparedWrite<'_> {
    /// Atomically remove one value.
    fn controller_u64(&mut self, val: u64) {
        let bh = &mut *self.unwrapped;
        let lh = &mut *self.wrapped;

        // We only ever dequeue 64 at once..
        assert!(bh.is_empty() || lh.is_empty());
        assert_eq!(bh.len() + lh.len(), 8);
        let val = u64::to_be_bytes(val);
        let bh = <&mut [u8; 8]>::try_from(bh);
        let lh = <&mut [u8; 8]>::try_from(lh);
        let buf = bh.or(lh).expect("logic bug");
        *buf = val;
    }

    fn commit(self) {
        if cfg!(debug_assertions) {
            let must_succeed = self.commit.inner.write_head.compare_exchange(
                self.write_base, self.write_end, Ordering::Release, Ordering::Relaxed);
            must_succeed.expect("Another thread accessed supposedly owned write head");
        } else {
            self.commit.inner.write_head.store(self.write_end, Ordering::Release);
        }
    }
}
