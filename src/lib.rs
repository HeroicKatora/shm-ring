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
//!
//! 2.2. Futex joining is done by competing for a particular member slot directly.
//! 2.2.1. Read the current number of members.
//! 2.2.2. Try to update the `futex` of the open page from `-1` to the `member` slot.
//! 2.2.3. On success, you've secured the spot. Wait for the controller to notice. It will unlock
//!   the futex to `-1` when it does. On failure, retry with an updated member count.
use core::{cell, mem, ptr};
use core::sync::atomic::{AtomicI32, AtomicU32};

use shared_memory::Shmem;

pub const MEMBER_LIMIT: usize = 256;

/// The monitor head is _only_ written by the controller.
#[repr(C)]
pub struct MonitorHead {
    /// Static version of this header structure.
    version: u32,
    /// A bit-mask of available methods for opening a new handle.
    /// `0x00000001`: wait by reading from a named pipe
    /// `0x00000002`: futex based waiting using the futex in the head (?? WIP details)
    /// `0x00000004`: wait using an semaphore (?? WIP details)
    open_with: u32,
    /// File-relative address of the page for new process to join.
    open_page: u64,
    /// File-relative address of the first page used by queues.
    queue_start: u64,
    /// File-relative address of the first page used for shared memory of queues.
    heap_start: u64,
    /// The number of current members.
    members: u32,
    /// The maximal number of members.
    max_members: u32,
    /// The start address of the pipe's name if any.
    pipe_name: u64,
    /// The start address of the member list. It has `max_members` entries.
    member_list: u64,
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

pub struct OpenOptions {
    _private: (),
}

pub struct ShmController {
    fd: std::fs::File,
    shared: Shmem,
    monitor: ptr::NonNull<cell::UnsafeCell<MonitorHead>>,
    open: ptr::NonNull<cell::UnsafeCell<MonitorOpen>>,
}

pub struct ShmClient {
    shared: Shmem,
    monitor: ptr::NonNull<cell::UnsafeCell<MonitorHead>>,
    open: ptr::NonNull<cell::UnsafeCell<MonitorOpen>>,
}

pub struct ShmRing {
    shared: Shmem,
    monitor: ptr::NonNull<cell::UnsafeCell<MonitorHead>>,
    open: ptr::NonNull<cell::UnsafeCell<MonitorOpen>>,
    private_ring: ptr::NonNull<QueueHead>,
}

pub struct ConnectError;

impl OpenOptions {
    pub fn new() -> Self {
        todo!()
    }

    pub fn open(path: &std::path::Path) -> std::io::Result<ShmClient> {
        todo!()
    }

    pub fn create() -> std::io::Result<ShmController> {
        todo!()
    }
}

impl ShmController {
    pub fn path(&self) -> &std::path::Path {
        todo!()
    }
}

impl ShmClient {
    pub fn connect(&mut self) -> Result<ShmRing, ConnectError> {
        todo!()
    }
}
