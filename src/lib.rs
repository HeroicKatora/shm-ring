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
struct ShmQueues {
    private_head: ptr::NonNull<QueueHead>,
    private_queues: ptr::NonNull<u64>,
    private_mem: ptr::NonNull<u8>,
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

pub struct ConnectError;
pub struct JoinError;
pub struct OpenError;

impl OpenOptions {
    const PAGE_SIZE: usize = 4096;

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
        for member in controller.member_list() {
            member.store(0, Ordering::Release);
        }

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

    /// Handle a round of messages.
    /// Will never block but might take a while?
    pub fn enter(&self) {
        // TODO:
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
    pub fn connect(self) -> Result<ShmControllerRing, ConnectError> {
        let allowed = self.monitor().open_with.load(Ordering::Relaxed);
        if self.join_with.as_u32() & allowed == 0 {
            return Err(ConnectError);
        }

        let pid = os_impl::own_pid();
        let this = self.find_open_client(pid)?;

        let queue = match self.join_with {
            JoinMethod::Pipe | JoinMethod::Semaphore => {
                self.nicely_rescind_client(this, pid);
                return Err(ConnectError);
            },
            JoinMethod::Futex => os_impl::futex(&self, this),
        };

        let this = queue?;
        let (private_head, private_queues, private_mem);
        // FIXME: validated pointers? We trust the server but it's a precaution against
        // alternative implementations that are wrong..
        private_head = self.heads.head_start(this);
        private_queues = self.heads.queue_start(this);
        private_mem = self.heads.heap_start(this);

        let queues = ShmQueues {
            private_head,
            private_queues,
            private_mem,
        };

        let ring = ShmRing {
            queues,
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
                match c.compare_exchange(0, pid, Ordering::Acquire, Ordering::Relaxed) {
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
}
