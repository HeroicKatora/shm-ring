//! An IPC task queue similar to io-uring.
//!
//! Use with any language capable of mapping anonymous files and accessing them as bytes.
//!
//! ## Layout
//!
//! The layout is defined via basic C-structures, assuming the simplest compliant layout. That is,
//! all types and fields are aligned, and fields are placed one after noahter with smallest offset.
//! Further, n-byte integers and atomics have exactly size of `n`. Alignments are never larger than
//! the size. All relevant structs are defined without interior padding (otherwise report a bug).
//!
//! At times we use the term vaddress.  This always means an address within the shared memory file,
//! i.e. an offset to the start of the memory mapped file if you use that mechanism.
//!
//! Currently Page always means `4096` bytes.
//!
//! - @0, one Page: A [`MonitorHead`], called `head`.
//! - @`head.open`, one Page: A [`MonitorOpen`], called `open`.
//! - @`head.head_start`, N Pages: Each page is a [`QueueHead`], starting with the pages for the
//!   `head.max_members` member slots. There is exactly one page per Queue. Queues are identified
//!   by their page offset, i.e. the queue at `head.head_start` is queue 0 and the one at
//!   `head.head_start + PAGE_SIZE` is queue 1.
//! - @`head.queue_start`, 2*N Pages: Each two pages are one queue with space for the A and B side.
//! - @`head.scratch_start`, 2*N Pages: Each two pages are one scratch space for the A and B side.
//! - @`head.member_list`: A vaddress pointing to a list of `AtomicU64` with `head.max_members`
//!   entries. Each entry can hold the PID of a client occupying that slot. See below.
//! - @`open.pipe_name`: If non-zero then a vaddress pointing to the name of the pipe for joining by
//!   pipe.
//!
//! [`MonitorHead`]: struct.MonitorHead.html
//! [`MonitorOpen`]: struct.MonitorOpen.html
//! [`QueueHead`]: struct.QueueHead.html
//!
//! ## Joining
//!
//! 1. Inspect the monitor head to determine a join method that is implemented.
//!    1. Might inspect the member count to see if joining is probable at all.
//! 2. Joining.
//!    The problem here is to distribute client identities. This process must not race between
//!    different clients (potential members). We may want some mediation as well. At the same time
//!    we need to notify the controller of our initial presence (which must setup any watch dog if
//!    we consider sudden exits). All of this must happen efficiently such that no spin lock is
//!    necessary. Basically we need an alternative to interrupts/syscalls here.
//!    1. The first step is to grab one client spot. This is done by atomically exchanging a client
//!       entry for your own process ID. This allows the server to detect if the process is gone. This
//!       has false positives if PIDs are reused but no false negatives so that no active state is
//!       destroyed. The next step depends on the synchronization method.
//!    2. Joining via a futex.
//!       1. Try to update the `futex` of the open page from `0` to your member ID.
//!       2. Wait for the controller to notice. It will unlock the futex to `0` when it does. On
//!          failure, another process interfered, retry.
//!    3. Joining via a pipe.
//!       1. Write the member ID as a Network Endian integer into the indicated pipe. Attach a
//!          process file desciptor of your own PID as out-of-band data if you want.
//!       2. I'm not sure how to ensure the server has read it. The server replying with that same
//!          int feels kind of racey if it gets buffered?
//!    4. Joining via a semaphore.
//!       1. Not yet specified.
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
mod atomic;
pub mod control;
mod controller;
#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod os_impl;
#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod os_impl;
pub mod process;

use core::convert::TryFrom;
use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering};
use core::{cell, marker, mem, ptr, slice};

use atomic::{AtomicMem, Slice};
use shared_memory::Shmem;

/// A bit mask marking unstable versions.
pub const FORMAT_IS_UNSTABLE: u32 = 0x8000_0000;
/// The current format version of the monitor head.
/// An unstable version (see `FORMAT_IS_UNSTABLE`) must match exactly because we take complete
/// freedom in the layout of the header, it's remaining bits are randomly generated once per bump.
//TODO: figure out if we can 'automatically' test for layout stability by doing some const-eval.
pub const FORMAT_VERSION: u32 = 2
    // FIXME: comment this line when it's ready.
    | FORMAT_IS_UNSTABLE | 0xffbd_833c
    // Line exists to nicely terminate the above expression.
    | 0;
/// An incompatible change will change some bit within this mask.
pub const FORMAT_COMPAT_MASK: u32 = 0x7fff_0000;

/// The magic number identifying an shm-ring shared memory file.
pub const SHM_MAGIC: u32 = 0x9d14_c252;
/// The maximum supported number of controlled members.
pub const MEMBER_LIMIT: u32 = OpenOptions::MEMBERS_MAX;
/// The maximum support number of controlled rings.
pub const RING_LIMIT: u32 = OpenOptions::RINGS_MAX;

/// The monitor head is _only_ written by the controller.
#[repr(C)]
pub struct MonitorHead {
    pub magic: AtomicU32,
    /// Static version of this header structure.
    pub version: AtomicU32,
    /// File-relative address of the page for new process to join.
    pub open_page: AtomicU64,
    /// File-relative address of the first page used by queue heads.
    pub head_start: AtomicU64,
    /// File-relative address of the first page used by queues.
    pub queue_start: AtomicU64,
    /// File-relative address of the first page used for shared memory of queues.
    pub scratch_start: AtomicU64,
    /// A bit-mask of available methods for opening a new handle.
    /// `0x00000001`: wait by reading from a named pipe
    /// `0x00000002`: futex based waiting using the futex in the head (?? WIP details)
    /// `0x00000004`: wait using an semaphore (?? WIP details)
    pub open_with: AtomicU32,
    /// The number of available queues in total.
    pub nr_queues: AtomicU32,
    /// The number of current members.
    pub members: AtomicU32,
    /// The maximal number of members.
    pub max_members: AtomicU32,
    /// The start address of the member list. It has `max_members` entries.
    pub member_list: AtomicU64,
    /// The number of pages per queue.
    /// This is also the stride within the array of queues starting at `queue_start`.
    pub pages_per_queue: AtomicU32,
    /// The number of pages per queue.
    /// This is also the stride within the array of scratch space starting at `scratch_start`.
    pub pages_per_scratch: AtomicU32,
}

/// The page with which new clients communicate with the controller.
#[repr(C)]
pub struct MonitorOpen {
    /// A shared register where a process can write its address.
    /// Must first hold the futex.
    pub tid_futex: u64,
    /// A shared register where a process can write its address.
    /// Must first hold the semaphore.
    pub tid_semaphore: u64,
    /// Futex to wait on for global control operations such as join.
    pub futex: AtomicI32,
    /// Filler. Unused. Don't use. Might get used.
    /// NOTE: This field is only pub for rustdoc.
    #[cfg(not(rustdoc))]
    pub _reserved: u32,
    #[cfg(rustdoc)]
    _reserved: u32,
    /// The start address of the pipe's name if any.
    pub pipe_name: AtomicU64,
}

/// A queue between two threads.
#[repr(C)]
pub struct QueueHead {
    /// The struct controlled by side A.
    pub half_a: QueueHalf,
    /// The struct controlled by side B.
    pub half_b: QueueHalf,
}

/// A reader from a queue.
pub struct ReadHalf<'queue> {
    inner: &'queue QueueHalf,
    buffer: &'queue [AtomicU8],
}

/// A reader from a queue.
pub struct WriteHalf<'queue> {
    inner: &'queue QueueHalf,
    buffer: &'queue [AtomicU8],
}

#[repr(C)]
pub struct QueueHalf {
    /// First futex that the threads may use.
    pub futex_a: AtomicI32,
    /// Filler. Unused. Don't use. Might get used.
    /// NOTE: This field is only pub for rustdoc.
    #[cfg(not(rustdoc))]
    pub _reserved: u32,
    #[cfg(rustdoc)]
    _reserved: u32,
    /// The write head.
    pub write_head: AtomicU32,
    /// The read head.
    pub read_head: AtomicU32,
    /// Private use data for this side.
    pub user_data: cell::UnsafeCell<u64>,
}

/// An enumeration (as mask bits) of available join methods.
///
/// See the crate level documentation for a description of each method and the motivation behind
/// doing one through the controller at all.
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum JoinMethod {
    /// If the bit is set then the controller allows joining by pipe.
    Pipe = 0x00000001,
    /// If the bit is set then the controller allows joining by futex.
    Futex = 0x00000002,
    /// If the bit is set then the controller allows joining by semaphore.
    Semaphore = 0x00000004,
}

/// A builder for a new shared memory controller or joining an existing one.
pub struct OpenOptions {
    num_rings: u32,
    max_members: u32,
    queue_pages: u32,
    scratch_pages: u32,
}

/// Valid pointers to a shm-ring head.
#[derive(Clone, Copy)]
struct ShmHeads {
    monitor: ptr::NonNull<MonitorHead>,
    open: ptr::NonNull<MonitorOpen>,
}

// these are part of the allocation for the process.
unsafe impl Send for ShmHeads {}

pub struct ShmController {
    shared: Shmem,
    heads: ShmHeads,
    state: Box<ControllerState>,
}

// FIXME: these should be derived, but shared_memory::MapData->shared_memory::Shmem fails to
// provide it and contains a single raw pointer.
unsafe impl Send for ShmController {}
unsafe impl Send for ShmClient {}
unsafe impl Send for ShmRing {}

pub use crate::controller::ControllerState;

pub struct Events {
    joined: Vec<(u64, u32)>,
    messages: Vec<(control::ControlMessage, control::ControlMessage)>,
}

#[non_exhaustive]
#[derive(Debug)]
pub enum Event {
    Joined {
        who: u64,
        on: u32,
    },
    Message {
        message: control::ControlMessage,
        answer: control::ControlMessage,
    },
}

/// A client looking to join the ring.
/// Note that you can join multiple times for independent control connections.
pub struct ShmClient {
    shared: Shmem,
    heads: ShmHeads,
    join_with: JoinMethod,
}

/// A single queue in the ring.
///
/// This assumes that the ring is alive and the pointers are all correct! Further it helps avoid
/// client errors from incorrectly assuming to be the owner of a queue. It's not that this could
/// lead to bad UB but rather that it could produce confusing state such a publishing a request
/// before all its information was written into the send queue half.
///
/// In other words, if you have more than one queue then the read or write heads might become
/// out-of-sync of the assumed range of bytes in the respective queues.
struct ShmQueues {
    private_head: ptr::NonNull<QueueHead>,
    private_queues: ptr::NonNull<u64>,
    private_mem: ptr::NonNull<u8>,
    pages_per_queue: u32,
    pages_per_scratch: u32,
    is_a_side: bool,
}

/// An active connection to another client (or the controller).
pub struct ShmRing {
    shared: Shmem,
    heads: ShmHeads,
    queues: ShmQueues,
    queue_id: ShmRingId,
}

pub struct ShmQueue<'shared_is_alive> {
    _lt: marker::PhantomData<&'shared_is_alive ShmRing>,
    heads: ShmHeads,
    queues: ShmQueues,
    id: ShmRingId,
}

/// A queue controlling a socket.
pub struct SocketQueue<'shared_is_alive> {
    queue: ShmQueue<'shared_is_alive>,
}

/// An active connection to the controller.
pub struct ShmControllerRing {
    /// The ring connecting to the controller.
    pub ring: ShmRing,
    /// The client id owned by this ring.
    this: ShmRingId,
    /// The pid we registered..
    pid: u64,
    _private: (),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShmRingId(pub u32);

pub struct TrustedQueues<'shared_is_alive> {
    inner: &'shared_is_alive ShmControllerRing,
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
#[derive(Debug)]
pub struct OptionsError;

impl OpenOptions {
    // FIXME: should use constant from libc or system headers.
    const PAGE_SIZE: usize = (1 << 12);
    // TODO: do we want a useful magic before release?
    const MAGIC: u32 = SHM_MAGIC;
    const RINGS_MAX: u32 = 4096;
    const MEMBERS_MAX: u32 = 256;
    /// Pages per queue in a ring.
    const DEFAULT_QUEUE_PAGES: u32 = 1 << 4;
    /// Number of pages of scratch space per queue.
    const DEFAULT_SCRATCH_PAGES: u32 = 1 << 4;

    pub fn new() -> Self {
        OpenOptions {
            num_rings: 64,
            max_members: 32,
            queue_pages: Self::DEFAULT_QUEUE_PAGES,
            scratch_pages: Self::DEFAULT_SCRATCH_PAGES,
        }
    }

    /// Configure the number of rings.
    ///
    /// The member count must be not be larger than the new number of rings to allocate and the
    /// ring count must not exceed a constant limit.
    pub fn with_rings(mut self, rings: u32) -> Result<Self, OptionsError> {
        if rings >= Self::RINGS_MAX {
            return Err(OptionsError);
        }

        if rings < self.max_members {
            return Err(OptionsError);
        }

        self.num_rings = rings;
        Ok(self)
    }

    /// Configure the number of maximum members.
    ///
    /// The new member count must be not be larger than the number of rings to allocate and not
    /// exceed a constant limit.
    pub fn with_members(mut self, members: u32) -> Result<Self, OptionsError> {
        if members >= Self::MEMBERS_MAX {
            return Err(OptionsError);
        }

        if self.num_rings < members {
            return Err(OptionsError);
        }

        self.max_members = members;
        Ok(self)
    }

    pub fn open(self, path: &str) -> Result<ShmClient, shared_memory::ShmemError> {
        let shared = shared_memory::ShmemConf::new().os_id(path).open()?;

        // Okay, can we begin inspecting the pages?
        if shared.len() < mem::size_of::<MonitorHead>() {
            return Err(shared_memory::ShmemError::MapSizeZero);
        }

        let head = ptr::NonNull::new(shared.as_ptr()).unwrap();
        let monitor_only: &MonitorHead = unsafe { &*head.cast().as_ptr() };

        // Check magic to ensure it's not a completely stupid shared memory.
        if monitor_only.magic.load(Ordering::Acquire) != OpenOptions::MAGIC {
            return Err(shared_memory::ShmemError::MapSizeZero);
        }

        // Now check version compatibility.
        let aparent_version = monitor_only.version.load(Ordering::Relaxed);

        if FORMAT_VERSION & FORMAT_IS_UNSTABLE != 0 {
            if aparent_version != FORMAT_VERSION {
                return Err(shared_memory::ShmemError::MapSizeZero);
            }
        } else {
            if aparent_version & FORMAT_COMPAT_MASK != FORMAT_VERSION & FORMAT_COMPAT_MASK {
                return Err(shared_memory::ShmemError::MapSizeZero);
            }
        }

        // Okay, now the current format specific checks.
        if shared.len() < 2 * mem::size_of::<MonitorHead>() {
            return Err(shared_memory::ShmemError::MapSizeZero);
        }

        let heads = unsafe {
            // SAFETY: we've somewhat validated this is the right shared memory. At least it's
            // large enough.
            ShmHeads::from_monitor(head.cast(), shared.len())
        }
        .map_err(|_| shared_memory::ShmemError::MapSizeZero)?;

        Ok(ShmClient {
            shared,
            heads,
            join_with: JoinMethod::Pipe,
        })
    }

    pub fn create(self, path: &str) -> Result<ShmController, shared_memory::ShmemError> {
        assert!(
            self.max_members <= self.num_rings,
            "At least one ring per potential member is required"
        );
        let pages = 3 // head pages for the controller.
            // pages for each ring control head
            + self.num_rings as usize * 1
            // pages for each rings' queues
            + self.num_rings as usize * 2 * self.queue_pages as usize
            // pages for each rings' scratch space
            + self.num_rings as usize * 2 * self.scratch_pages as usize;
        let size = pages * Self::PAGE_SIZE;

        let mut shared = shared_memory::ShmemConf::new()
            .os_id(path)
            // Three pages for the controller: head, open, free
            // Five pages per ring (one head, 2 for commands, 2 buffers)
            .size(size)
            .create()?;
        assert!(shared.len() >= size);
        shared.set_owner(true);
        let (monitor, open);
        unsafe {
            monitor = ptr::NonNull::new(shared.as_ptr()).unwrap();
            open = ptr::NonNull::new(monitor.as_ptr().add(Self::PAGE_SIZE)).unwrap();
        }

        let heads = ShmHeads {
            monitor: monitor.cast(),
            open: open.cast(),
        };

        let state = ControllerState::new(&self, &heads);

        let controller = ShmController {
            shared,
            heads,
            state,
        };

        // Init sequence: write static information and lastly write the open_with information
        let monitor = controller.monitor();
        monitor.magic.store(OpenOptions::MAGIC, Ordering::Release);
        monitor.version.store(FORMAT_VERSION, Ordering::Release);
        monitor
            .open_page
            .store(Self::PAGE_SIZE as u64, Ordering::Release);
        let first_head = 3;
        monitor
            .head_start
            .store(first_head * Self::PAGE_SIZE as u64, Ordering::Release);
        let first_queue = first_head + u64::from(self.num_rings);
        monitor
            .queue_start
            .store(first_queue * Self::PAGE_SIZE as u64, Ordering::Release);
        let first_heap = first_queue + u64::from(self.num_rings) * 2 * u64::from(self.queue_pages);
        monitor
            .scratch_start
            .store(first_heap * Self::PAGE_SIZE as u64, Ordering::Release);
        monitor.nr_queues.store(self.num_rings, Ordering::Release);
        monitor.members.store(0, Ordering::Release);
        monitor
            .max_members
            .store(self.max_members, Ordering::Release);
        // For now, starts directly behind head.
        let member_list_start = u64::try_from(mem::size_of::<MonitorHead>()).unwrap();
        monitor
            .member_list
            .store(member_list_start, Ordering::Release);
        monitor
            .pages_per_queue
            .store(self.queue_pages, Ordering::Release);
        monitor
            .pages_per_scratch
            .store(self.scratch_pages, Ordering::Release);
        assert_eq!(controller.member_list().len(), self.max_members as usize);
        for member in controller.member_list() {
            member.store(0, Ordering::Release);
        }

        let open = controller.open();
        open.futex.store(-1, Ordering::Release);
        open.pipe_name.store(0, Ordering::Release);

        // Finally, store the join methods.
        monitor
            .open_with
            .store(os_impl::join_methods(), Ordering::Release);

        Ok(controller)
    }
}

impl ShmController {
    pub fn raw_join_methods(&self) -> u32 {
        self.monitor().open_with.load(Ordering::Relaxed)
    }

    pub fn members(&self) -> (u32, u32) {
        self.monitor().members()
    }

    /// Handle a round of messages.
    /// Will never block but might take a while?
    pub fn enter(&mut self) {
        self.handle_new_client(None);
        self.handle_client_queues(None);
    }

    /// Enter and collect/trace events that happened.
    pub fn enter_with_events(&mut self, events: &mut Events) {
        self.handle_new_client(Some(events));
        self.handle_client_queues(Some(events));
    }

    fn handle_new_client(&self, mut events: Option<&mut Events>) {
        os_impl::join_futex(self, |request| {
            let who = self.member_list()[request as usize].load(Ordering::Relaxed);

            if let Some(ref mut events) = events {
                events.who_joined_on(who, request);
            }

            self.heads
                .queues_controller(ShmRingId(request))
                .queues
                .reset();
            self.monitor().members.fetch_add(1, Ordering::Relaxed);
        });
    }

    fn handle_client_queues(&mut self, mut events: Option<&mut Events>) {
        let clients = self.monitor().max_members.load(Ordering::Relaxed);
        for i in 0..clients {
            let heads = self.heads;
            let client_id = controller::ClientId(i);
            let queue = heads.queues_controller(ShmRingId(i));
            let events = events.as_mut().map(|e| &mut **e);
            self.state.execute_control(client_id, queue, events)
        }

        let sockets = self.state.sockets.len();
        for i in 0..sockets {
            let ring_id = self.state.sockets[i].queue;
            let socket_id = controller::SocketId(i as u32);
            let heads = self.heads;
            let queue = heads.queues_controller(ShmRingId(ring_id.0));
            let events = events.as_mut().map(|e| &mut **e);
            self.state.execute_socket(socket_id, queue, events)
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

impl ControllerState {
    fn new(config: &OpenOptions, _: &ShmHeads) -> Box<Self> {
        Box::new(ControllerState {
            free_queues: (config.max_members..config.num_rings)
                .map(controller::QueueId)
                .collect(),
            open_server: vec![],
            active: vec![],
            open_client: vec![],
            sockets: vec![],
        })
    }
}

impl Events {
    /// Clear events, return how many were ignored.
    pub fn clear(&mut self) -> usize {
        let ignored = self.joined.len() + self.messages.len();
        self.joined.clear();
        self.messages.clear();
        ignored
    }

    pub fn pop(&mut self) -> Option<Event> {
        if let Some((who, on)) = self.joined.pop() {
            return Some(Event::Joined { who, on });
        }

        if let Some((message, answer)) = self.messages.pop() {
            return Some(Event::Message { message, answer });
        }

        None
    }

    fn who_joined_on(&mut self, who: u64, on: u32) {
        self.joined.push((who, on))
    }

    fn request(&mut self, what: control::ControlMessage, answer: control::ControlMessage) {
        self.messages.push((what, answer))
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

        let queue = match self.join_with {
            JoinMethod::Pipe | JoinMethod::Semaphore => {
                self.nicely_rescind_client(want, pid);
                return Err(ConnectError);
            }
            JoinMethod::Futex => os_impl::futex(&self, want),
        };

        let this = ShmRingId(queue?);

        let ring = ShmRing {
            queues: self.heads.queues_controller(this).reverse().queues,
            shared: self.shared,
            heads: self.heads,
            queue_id: this,
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
        Ok(ShmClient { shared, ..*self })
    }

    fn find_open_client(&self, pid: u64) -> Result<u32, ConnectError> {
        let clients = self.member_list();

        loop {
            for (idx, c) in clients.iter().enumerate() {
                match c.compare_exchange(0, pid, Ordering::AcqRel, Ordering::Relaxed) {
                    Ok(_) => return Ok(idx as u32),
                    Err(_) => {}
                }
            }

            return Err(ConnectError);
        }
    }

    fn nicely_rescind_client(&self, this: u32, pid: u64) {
        let clients = self.member_list();
        // We should be the only updating this right now. So ignore errors..
        // This would indicate someone else already has grabbed control. Sure.
        let _ =
            clients[this as usize].compare_exchange(pid, 0, Ordering::Relaxed, Ordering::Relaxed);
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
    /// Clone the queue, which we borrow for the lifetime.
    ///
    /// Since we borrow ourselves mutably and the exposed type requires mutable references as well,
    /// this will only do the expected things.
    pub fn queue(&mut self) -> ShmQueue<'_> {
        self.queue_from_ref()
    }

    /// Do the same as queue but allows multiple refs.
    /// You must call this through a `Trusted*` interface for your own sanity's sake.
    pub(crate) fn queue_from_ref(&self) -> ShmQueue<'_> {
        ShmQueue {
            _lt: marker::PhantomData,
            heads: self.heads,
            queues: self.queues.get_a_clone_okay_just_give_me(),
            id: self.queue_id,
        }
    }
}

impl ShmQueue<'_> {
    /// Send one chunk of data.
    pub fn send(&mut self, data: &[u8]) -> Result<(), SendError> {
        self.send_with(data, |_, _| ())
    }

    /// Send one chunk of data with a callback on success before committing.
    ///
    /// The callback may perform more expensive work that can be elided in the unsuccessful cases.
    /// It is only called when enough buffer space was available.
    pub fn send_with<T>(
        &mut self,
        data: &[u8],
        pre_commit: impl FnOnce(&[u8], &WriteHalf) -> T,
    ) -> Result<T, SendError> {
        let len = u32::try_from(data.len()).map_err(|_| SendError)?;
        let mut writer = self.queues.half_to_write_to();

        let write = writer.prepare_write(len).ok_or(SendError)?;
        let (lhs, rhs) = data.split_at(write.unwrapped.len());
        lhs.copy_to_relaxed(&write.unwrapped);
        rhs.copy_to_relaxed(&write.wrapped);
        let result = pre_commit(data, write.commit);
        write.commit();

        Ok(result)
    }

    /// Receive one chunk of data.
    pub fn recv(&mut self, data: &mut [u8]) -> Result<(), RecvError> {
        self.recv_with(data, |_, _| ())
    }

    /// Receive one chunk of data, with some code that can run before the receive is committed.
    ///
    /// This allows one to read auxiliary data from the associated memory (or some other location)
    /// before the other side of the queue assumes that the request has been handled and
    /// potentially updates the data. Note that none of reads/writes should be races but the
    /// results would be unexpected or corrupted.
    pub fn recv_with<T>(
        &mut self,
        data: &mut [u8],
        pre_commit: impl FnOnce(&mut [u8], &ReadHalf) -> T,
    ) -> Result<T, RecvError> {
        let len = u32::try_from(data.len()).map_err(|_| RecvError)?;
        let mut reader = self.queues.half_to_read_from();

        let read = reader.available(len).ok_or(RecvError)?;
        let (lhs, rhs) = data.split_at_mut(read.unwrapped.len());
        lhs.copy_from_relaxed(&read.unwrapped);
        rhs.copy_from_relaxed(&read.wrapped);
        let result = pre_commit(data, read.commit);
        read.commit();

        Ok(result)
    }

    /// Get the id that identifies the queue in the `shm-ring`.
    pub fn queue_id(&self) -> ShmRingId {
        self.id
    }

    pub(crate) fn reverse(self) -> Self {
        ShmQueue {
            queues: self.queues.reverse(),
            ..self
        }
    }
}

impl SocketQueue<'_> {
    pub fn request(&mut self, msg: impl Into<control::SocketMessage>) -> Result<(), SendError> {
        let msg = msg.into().op.to_be_bytes();
        self.queue.send(&msg)
    }

    pub fn response<T>(
        &mut self,
        handler: impl FnOnce(control::ControlResponse) -> T,
    ) -> Result<T, RecvError> {
        let mut op = [0; 8];
        self.queue.recv_with(&mut op, |filled, ring| {
            let op = <&mut [u8; 8]>::try_from(filled).unwrap();
            let resp = control::ControlResponse::new(ring, *op);
            handler(resp)
        })
    }
}

impl ShmControllerRing {
    /// The id of this client (and its ring).
    pub fn client_id(&self) -> ShmRingId {
        self.this
    }

    /// Get an intermediate with which you can access another queue.
    ///
    /// Other queues may be those that were granted by the controller or those which this client
    /// joined.
    ///
    /// This relies on the fact that accessing another's queue (or another accessing ours) is not
    /// fundamentally unsound. If you use the wrong queue you may cause a large amount of chaos but
    /// it won't be UB directly. Please don't do it.
    pub fn trust_me_with_all_queues(&self) -> TrustedQueues<'_> {
        TrustedQueues { inner: &self }
    }

    /// Send a request to the controller.
    pub fn request(&mut self, msg: impl Into<control::ControlMessage>) -> Result<(), SendError> {
        let msg = msg.into().op.to_be_bytes();
        self.ring.queue().send(&msg)
    }

    /// Receive one response if there is one available.
    pub fn response<T>(
        &mut self,
        handler: impl FnOnce(control::ControlResponse) -> T,
    ) -> Result<T, RecvError> {
        let mut op = [0; 8];
        self.ring.queue().recv_with(&mut op, |filled, ring| {
            let op = <&mut [u8; 8]>::try_from(filled).unwrap();
            let resp = control::ControlResponse::new(ring, *op);
            handler(resp)
        })
    }
}

impl<'shared_is_alive> TrustedQueues<'shared_is_alive> {
    pub fn as_server(&self, id: ShmRingId) -> ShmQueue<'shared_is_alive> {
        let ring = &self.inner.ring;

        // Just a sanity test. This shouldn't refer to a member queue which can not be the server.
        let members = ring.heads.monitor().max_members.load(Ordering::Relaxed);
        assert!(
            members < id.0,
            "That queue belongs to a member. Call as_member instead."
        );

        ring.heads.queues_controller(id)
    }

    pub fn as_client(&self, id: ShmRingId) -> ShmQueue<'shared_is_alive> {
        let ring = &self.inner.ring;

        // Just a sanity test. This shouldn't refer to a member queue which can not be the server.
        let members = ring.heads.monitor().max_members.load(Ordering::Relaxed);
        assert!(
            members < id.0,
            "That queue belongs to a member. Call as_member instead."
        );

        ring.heads.queues_controller(id).reverse()
    }

    pub fn as_socket(&self, id: ShmRingId) -> SocketQueue<'shared_is_alive> {
        let ring = &self.inner.ring;
        let queue = ring.heads.queues_controller(id);
        SocketQueue { queue }
    }
}

impl ShmHeads {
    /// Open a heads page.
    /// # Safety
    /// Caller must guarantee that `ptr` can be dereferenced at least one page and that it points
    /// to a shm-ring shared memory page that has been mmap'ed into contiguous memory. Then this
    /// code will check all interior invariants to sanity check the whole thing.
    pub(crate) unsafe fn from_monitor(
        ptr: ptr::NonNull<MonitorHead>,
        len: usize,
    ) -> Result<Self, OpenError> {
        // Check offsets against the total length.
        struct PageBound(u64);

        impl PageBound {
            fn from_length(len: u64) -> Self {
                PageBound(len)
            }
            fn fits_pages_at(&self, at: u64, pages: u64) -> Result<(), OpenError> {
                let page_size = u64::try_from(OpenOptions::PAGE_SIZE).unwrap();
                if at & (page_size - 1) != 0 {
                    return Err(OpenError);
                }
                let page_required = pages.checked_mul(page_size).ok_or(OpenError)?;
                self.fits_length_at(at, page_required)
            }
            fn fits_length_at(&self, at: u64, required: u64) -> Result<(), OpenError> {
                if self.0.checked_sub(required) < Some(at) {
                    return Err(OpenError);
                }
                Ok(())
            }
        }

        // SAFETY: caller guarantees at least one valid page.
        let monitor = &*ptr.as_ptr();
        let length = match u64::try_from(len) {
            Err(_) => return Err(OpenError),
            Ok(length) => length,
        };

        let page_bound = PageBound::from_length(length);

        if monitor.open_with.load(Ordering::Acquire) == 0 {
            // Oops, not yet initialized.
            return Err(OpenError);
        }

        let open_offset = monitor.open_page.load(Ordering::Acquire);
        page_bound.fits_pages_at(open_offset, 1)?;
        let nr_queues = monitor.nr_queues.load(Ordering::Acquire);
        let heads = monitor.head_start.load(Ordering::Acquire);
        page_bound.fits_pages_at(heads, u64::from(nr_queues))?;
        let queues = monitor.queue_start.load(Ordering::Acquire);
        page_bound.fits_pages_at(queues, 2 * u64::from(nr_queues))?;
        let scratch = monitor.scratch_start.load(Ordering::Acquire);
        page_bound.fits_pages_at(scratch, 2 * u64::from(nr_queues))?;

        let max_members = monitor.max_members.load(Ordering::Acquire);
        let member_list = monitor.member_list.load(Ordering::Acquire);
        page_bound.fits_length_at(member_list, u64::from(max_members) * u64::from(8u8))?;

        if max_members > nr_queues {
            return Err(OpenError);
        }

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
        unsafe { &*(self.open.as_ptr() as *const MonitorOpen) }
    }

    pub(crate) fn member_list(&self) -> &[AtomicU64] {
        let space = OpenOptions::PAGE_SIZE - mem::size_of::<MonitorHead>();
        let max_members = self.monitor().max_members.load(Ordering::Relaxed) as usize;
        assert!(
            space / mem::size_of::<AtomicU64>() >= max_members,
            "Not enough space for inline member list. Should have caught this in constructor"
        );
        let base = self.monitor.as_ptr();
        unsafe {
            // SAFETY: surely short enough, and enough space for max_members as validated above.
            let memlist = (base as *const MonitorHead).add(1) as *const AtomicU64;
            slice::from_raw_parts(memlist, max_members)
        }
    }

    /// Safe version of raw_queues_controller, does the invariant checks.
    pub(crate) fn queues_controller(&self, this: ShmRingId) -> ShmQueue<'_> {
        let nr_queues = self.monitor().nr_queues.load(Ordering::Relaxed);
        assert!(this.0 < nr_queues, "That queue does not exist.");
        // Safety:
        // * The val does not outlive self which outlives the shared memory.
        // * We've just asserted it is a valid queue.
        let queues = unsafe { self.raw_queues_controller(this) };
        ShmQueue {
            _lt: marker::PhantomData,
            heads: *self,
            queues,
            id: this,
        }
    }

    fn head_start(&self, this: ShmRingId) -> ptr::NonNull<QueueHead> {
        let base = self.monitor.as_ptr() as *mut u8;

        let start = self.monitor().head_start.load(Ordering::Relaxed) as usize;
        let offset = (this.0 as usize) * OpenOptions::PAGE_SIZE;

        unsafe { ptr::NonNull::new(base.add(start).add(offset) as *mut QueueHead).unwrap() }
    }

    fn queue_start(&self, this: ShmRingId) -> (ptr::NonNull<u64>, u32) {
        let base = self.monitor.as_ptr() as *mut u8;

        let start = self.monitor().queue_start.load(Ordering::Relaxed) as usize;
        let pages = self.monitor().pages_per_queue.load(Ordering::Relaxed);
        let offset = 2 * pages as usize * (this.0 as usize) * OpenOptions::PAGE_SIZE;

        let head = unsafe { ptr::NonNull::new(base.add(start).add(offset) as *mut u64).unwrap() };

        (head, pages)
    }

    fn scratch_start(&self, this: ShmRingId) -> (ptr::NonNull<u8>, u32) {
        let base = self.monitor.as_ptr() as *mut u8;

        let start = self.monitor().head_start.load(Ordering::Relaxed) as usize;
        let pages = self.monitor().pages_per_scratch.load(Ordering::Relaxed);
        let offset = 2 * pages as usize * (this.0 as usize) * OpenOptions::PAGE_SIZE;

        let head = unsafe { ptr::NonNull::new(base.add(start).add(offset)).unwrap() };

        (head, pages)
    }

    /// Create a pair of queues.
    ///
    /// # Safety
    ///
    /// * The pair must not outlive the shared memory.
    /// * The `this` index must be smaller than the `nr_queues`.
    unsafe fn raw_queues_controller(&self, this: ShmRingId) -> ShmQueues {
        // FIXME: validated pointers? We trust the server but it's a precaution against
        // alternative implementations that are wrong..
        let private_head = self.head_start(this);
        let (private_queues, pages_per_queue) = self.queue_start(this);
        let (private_mem, pages_per_scratch) = self.scratch_start(this);

        ShmQueues {
            private_head,
            private_queues,
            private_mem,
            pages_per_queue,
            pages_per_scratch,
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

    /// Call when you're sure it's fine to clone this.
    ///
    /// For example because the original is borrowed for the lifetime of the returned value or
    /// because the queue has been assumed to have been transferred to us.
    fn get_a_clone_okay_just_give_me(&self) -> Self {
        ShmQueues { ..*self }
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
        let memory_per_queue = self.pages_per_queue as usize * OpenOptions::PAGE_SIZE;
        let (inner, offset) = if self.is_a_side {
            (&queue.half_b, memory_per_queue)
        } else {
            (&queue.half_a, 0)
        };
        let buffer = unsafe { Self::queue_memory(self.private_queues, offset, memory_per_queue) };
        ReadHalf { inner, buffer }
    }

    fn half_to_write_to(&mut self) -> WriteHalf<'_> {
        let queue = unsafe { &*self.private_head.as_ptr() };
        let memory_per_queue = self.pages_per_queue as usize * OpenOptions::PAGE_SIZE;
        let (inner, offset) = if self.is_a_side {
            (&queue.half_a, 0)
        } else {
            (&queue.half_b, memory_per_queue)
        };
        let buffer = unsafe { Self::queue_memory(self.private_queues, offset, memory_per_queue) };
        WriteHalf { inner, buffer }
    }

    fn halves(&mut self) -> (ReadHalf<'_>, WriteHalf<'_>) {
        let queue = unsafe { &*self.private_head.as_ptr() };
        let memory_per_queue = self.pages_per_queue as usize * OpenOptions::PAGE_SIZE;

        let (read, roffset, write, woffset) = if self.is_a_side {
            (&queue.half_b, memory_per_queue, &queue.half_a, 0)
        } else {
            (&queue.half_a, 0, &queue.half_b, memory_per_queue)
        };

        let read_buffer =
            unsafe { Self::queue_memory(self.private_queues, roffset, memory_per_queue) };
        let write_buffer =
            unsafe { Self::queue_memory(self.private_queues, woffset, memory_per_queue) };

        let read_half = ReadHalf {
            inner: read,
            buffer: read_buffer,
        };

        let write_half = WriteHalf {
            inner: write,
            buffer: write_buffer,
        };

        (read_half, write_half)
    }

    unsafe fn queue_memory<'a>(
        queues: ptr::NonNull<u64>,
        offset: usize,
        memory_per_queue: usize,
    ) -> &'a [AtomicU8] {
        let addr = (queues.as_ptr() as *mut u8).add(offset);
        let buffer = ptr::slice_from_raw_parts(addr, memory_per_queue);
        &*(buffer as *const [AtomicU8])
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
    unwrapped: AtomicMem<'buffer>,
    wrapped: AtomicMem<'buffer>,
    read_base: u32,
    read_end: u32,
}

struct PreparedWrite<'buffer> {
    /// Access to the underlying writer.
    commit: &'buffer WriteHalf<'buffer>,
    unwrapped: AtomicMem<'buffer>,
    wrapped: AtomicMem<'buffer>,
    write_base: u32,
    write_end: u32,
}

impl ReadHalf<'_> {
    fn get_read_head_and_ready_bytes(&self) -> (u32, u32) {
        // On success we must have read with Acquire to sync the buffer contents.
        let writer = self.inner.write_head.load(Ordering::Acquire);
        // But not this, this is published by us..
        let reader = self.inner.read_head.load(Ordering::Relaxed);
        let modulo = OpenOptions::PAGE_SIZE as u32;

        let ready = ((writer + modulo) - reader) % modulo;
        (reader, ready)
    }

    /// Return (reader, ready) if it is at least count bytes.
    fn acquire_ready(&self, count: u32) -> Option<(u32, u32)> {
        let (reader, ready) = self.get_read_head_and_ready_bytes();

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

        let buffer = &self.buffer[..OpenOptions::PAGE_SIZE];
        let first = &buffer[reader as usize..norm_half as usize];
        let wrapped = &buffer[..wrap_half as usize];

        Some(PreparedRead {
            commit: &*self,
            read_base: reader,
            read_end: (reader + count) % modulo,
            unwrapped: first.into(),
            wrapped: wrapped.into(),
        })
    }
}

impl PreparedRead<'_> {
    /// Atomically remove one value.
    fn controller_u64(&self) -> u64 {
        let bh = &*self.unwrapped;
        let lh = &*self.wrapped;

        // We only ever dequeue 64 at once..
        assert!(bh.is_empty() || lh.is_empty());
        assert_eq!(bh.len() + lh.len(), 8);
        let bh = <&[AtomicU8; 8]>::try_from(bh);
        let lh = <&[AtomicU8; 8]>::try_from(lh);
        let val = bh.or(lh).expect("logic bug");
        let mut bytes = [0; 8];
        bytes.copy_from_relaxed(val);
        u64::from_be_bytes(bytes)
    }

    fn commit(self) {
        if cfg!(debug_assertions) {
            let must_succeed = self.commit.inner.read_head.compare_exchange(
                self.read_base,
                self.read_end,
                Ordering::Release,
                Ordering::Relaxed,
            );
            must_succeed.expect("Another thread accessed supposedly owned read head");
        } else {
            self.commit
                .inner
                .read_head
                .store(self.read_end, Ordering::Release);
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
        if empty < count {
            // Strict, new end must be smaller than reader.
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

        // SAFETY: synchronized as relaxed so we don't need synchronization to avoid UB. However,
        // the information is only coherent if the other thread has released this.
        let buffer = &self.buffer[..OpenOptions::PAGE_SIZE];
        let first = &buffer[writer as usize..norm_half as usize];
        let wrapped = &buffer[..wrap_half as usize];

        Some(PreparedWrite {
            commit: &*self,
            write_base: writer,
            write_end: (writer + count) % modulo,
            unwrapped: first.into(),
            wrapped: wrapped.into(),
        })
    }
}

impl PreparedWrite<'_> {
    /// Atomically remove one value.
    fn controller_u64(&mut self, val: u64) {
        let bh = &*self.unwrapped;
        let lh = &*self.wrapped;

        // We only ever dequeue 64 at once..
        assert!(bh.is_empty() || lh.is_empty());
        assert_eq!(bh.len() + lh.len(), 8);
        let val = u64::to_be_bytes(val);
        let bh = <&[AtomicU8; 8]>::try_from(bh);
        let lh = <&[AtomicU8; 8]>::try_from(lh);
        let buf = bh.or(lh).expect("logic bug");
        val.copy_to_relaxed(buf);
    }

    fn commit(self) {
        // eprintln!("{:p} {}", &self.commit.inner.write_head, self.write_end);
        if cfg!(debug_assertions) {
            let must_succeed = self.commit.inner.write_head.compare_exchange(
                self.write_base,
                self.write_end,
                Ordering::Release,
                Ordering::Relaxed,
            );
            must_succeed.expect("Another thread accessed supposedly owned write head");
        } else {
            self.commit
                .inner
                .write_head
                .store(self.write_end, Ordering::Release);
        }
    }
}
