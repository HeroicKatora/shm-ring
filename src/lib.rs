//! An IPC task queue similar to io-uring.
//!
//! Use with any language capable of mapping anonymous files and accessing them as bytes.
//!
//! ## Joining
//!
//! 1. Inspect the monitor head to determine a join method that is implemented.
//! 1.1. Might inspect the member count to see if joining is possible at all.
//!
//! 2.1. Pipe joining:
//! 2.1.1. Grab the name of the pipe from the open page.
//! 2.1.2. Read one byte from the pipe. This is the member index.
//! 2.1.3. Realize that the controller can not monitor your process's health right now.
//! 2.1.4. Try to tell it your TID before you crash (see monitor request).
//!
//! 2.2. Futex joining is done by competing for a particular member slot directly.
//! 2.2.1. Read the current number of members.
//! 2.2.2. Try to update the `futex` of the open page from `0` to your `TID`.
//! 2.2.3. On success, you've secured the spot. Wait for the controller to notice. It will unlock
//!   the futex to `0` when it does. On failure, retry. The scan the member list for your assigned
//!   spot.
pub mod control;

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

enum JoinMethod {
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

pub struct ShmClient {
    shared: Shmem,
    heads: ShmHeads,
    join_with: JoinMethod,
}

pub struct ShmRing {
    shared: Shmem,
    monitor: ptr::NonNull<MonitorHead>,
    open: ptr::NonNull<MonitorOpen>,
    private_ring: ptr::NonNull<QueueHead>,
}

pub struct OpenError;
pub struct ConnectError;

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

    pub fn create(self) -> Result<ShmController, shared_memory::ShmemError> {
        assert!(self.max_members <= self.num_rings, "At least one ring per potential member is required");
        let pages = self.num_rings as usize * 5 + 3;
        let size = pages * Self::PAGE_SIZE;
        let shared = shared_memory::ShmemConf::new()
            // Three pages for the controller: head, open, free
            // Five pages per ring (one head, 2 for commands, 2 buffers)
            .size(size)
            .force_create_flink()
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
        let first_queue = 3 + u64::from(self.num_rings);
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
    pub fn connect(&mut self) -> Result<ShmRing, ConnectError> {
        todo!()
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
}
