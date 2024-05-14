use alloc::sync::Weak;
use core::{cell, mem, ops, ptr};

use hashbrown::{hash_map, HashMap, HashSet};

use crate::{data, frame, uapi};

pub struct Server {
    /// Critical for safety, keeps the reference mappings in `server` alive.
    #[allow(dead_code)]
    ring: frame::Shared,
    server: ServerMap,
    /// For our inner sanity check, holding on to this informs the shared open mmap that a server
    /// is running in our process. Re-using `Weak` since we do not have any use for an actual weak
    /// reference to the `Arc` contents.
    #[allow(dead_code)]
    owner: Weak<dyn frame::RetainedMemory>,
}

/// A handler for a server, running its obligated tasks.
#[derive(Default)]
pub struct ServerTask {
    tracker: HashMap<data::ClientIdentifier, ClientTrackingData>,
}

struct ClientTrackingData {
    pid: uapi::OwnedFd,
    rings: HashSet<(usize, data::ClientSide)>,
}

#[derive(Clone, Copy)]
struct ServerMap {
    /// The frozen head.
    pub head: &'static data::ShmHead,
    pub info: &'static data::Rings,
    pub data: &'static cell::UnsafeCell<[u8]>,
}

pub struct ServerConfig<'lt> {
    pub vec: &'lt [RingConfig],
}

#[derive(Clone, Copy)]
// This is a constant, only has one allowed value, but we do not care. Might change or not.
#[allow(dead_code)]
pub struct RingVersion(u64);

#[derive(Debug)]
pub enum ServerError {
    TooSmall,
    SizeError,
    /// All rings must be divisible by their count.
    RingSizeError,
    /// All rings must contain entry counts which are powers-of-two.
    RingCountError,
    /// All entries must be powers-of-two.
    EntrySizeError,
}

pub struct RingConfig {
    pub version: RingVersion,
    pub ring_size: u64,
    pub data_size: u64,
    pub slot_entry_size: u64,
    pub rhs: i32,
    pub lhs: i32,
}

impl Server {
    const PAGE_SIZE: u64 = 4096;

    /// Walk the ring info block, collecting PID file descriptors where necessary. It is the job of
    /// the server to, eventually, deactivate the slot entries of crashed processes.
    pub fn collect_fds(&self, track: &mut ServerTask) {
        for (idx, info) in self.server.info.into_iter().enumerate() {
            if let Ok(client) = info.lhs.inspect() {
                track.track_client(client, idx, data::ClientSide::Left);
            }

            if let Ok(client) = info.rhs.inspect() {
                track.track_client(client, idx, data::ClientSide::Right);
            }
        }
    }

    /// Initialize a server, if the configuration checks out.
    ///
    /// # Safety
    ///
    /// Must be called by [`frame::Shared::into_server`], using all its precondition.
    pub(crate) unsafe fn new(ring: frame::Shared, cfg: ServerConfig) -> Result<Self, ServerError> {
        let info_offset = ring.tail_offset();
        let info_tail = ring.tail();

        // Safety: we store the shared memory.
        let info_tail = unsafe { &*info_tail };
        let size = mem::size_of_val(info_tail);

        let ring_offset = data::align_offset::<data::RingInfo>(info_tail.get().cast());
        let size = size.checked_sub(ring_offset).ok_or(ServerError::TooSmall)?;
        let offset = info_offset + ring_offset;
        // Safety: size check we just did with checked_sub.
        let info_tail = unsafe { (info_tail.get() as *const u8).add(ring_offset) };
        debug_assert_eq!(data::align_offset::<data::RingInfo>(info_tail), 0);

        let rings_len = cfg.vec.len() * mem::size_of::<data::RingInfo>();
        let pages_head = Self::page_requirement(rings_len)?;
        let rings_len = (pages_head * Self::PAGE_SIZE) as usize;

        // Safety: We haven't pulled up the info yet in `init`, hence this is fully under our
        // control. Taking a mutable reference makes it easy to initialize.
        let rings = info_tail as *mut data::RingInfo;
        let rings = ptr::slice_from_raw_parts_mut(rings, cfg.vec.len());

        let tail = unsafe { info_tail.add(rings_len) };
        let tail_size = size - rings_len;
        let offset = offset + rings_len;

        let end_offset = (size + offset).try_into().unwrap_or(u64::MAX);
        let offset = offset.try_into().unwrap_or(u64::MAX);
        Self::offsets(unsafe { &mut *rings }, cfg.vec, offset..end_offset)?;

        let tail = ptr::slice_from_raw_parts(tail, tail_size);
        let head = data::ShmHead {
            ring_magic: data::RingMagic::new(),
            ring_offset: ring_offset
                .try_into()
                .expect("Bounded by size_of::<RingInfo>"),
            ring_count: cfg.vec.len() as u64,
            ring_ping: data::RingPing::default(),
        };

        // Safety: we are known to be the only owner, since this `new` is only called when the
        // caller has already promised it.
        let (head, owner) = unsafe { ring.init(head) };

        // Safety: all properly initialized at this point.
        let server = ServerMap {
            head: unsafe { &*head },
            info: unsafe { &*(rings as *const data::Rings) },
            data: unsafe { &*(tail as *const cell::UnsafeCell<[u8]>) },
        };

        Ok(Server {
            ring,
            server,
            owner,
        })
    }

    pub fn bring_up(&self, ids: &[RingConfig]) -> usize {
        let mut success = 0;

        for (slot, ids) in self.server.info.into_iter().zip(ids) {
            if let Some(id) = data::RingIdentifier::new(ids.rhs) {
                success += usize::from(slot.rhs.reinit(id).is_ok());
            }

            if let Some(id) = data::RingIdentifier::new(ids.lhs) {
                success += usize::from(slot.lhs.reinit(id).is_ok());
            }
        }

        success
    }

    pub(crate) fn shared_ring(&self) -> &frame::Shared {
        &self.ring
    }

    pub(crate) fn head(&self) -> &data::ShmHead {
        &self.server.head
    }

    fn offsets(
        info: &mut [data::RingInfo],
        cfg: &[RingConfig],
        mut range: ops::Range<u64>,
    ) -> Result<(), ServerError> {
        fn allocate(range: &mut ops::Range<u64>, len: u64) -> Result<u64, ServerError> {
            let start = range.start;

            if range.end.checked_sub(start) < Some(len) {
                return Err(ServerError::TooSmall)?;
            }

            range.start += len;
            Ok(start)
        }

        if range.start % Self::PAGE_SIZE != 0 {
            let offset = Self::PAGE_SIZE - range.start % Self::PAGE_SIZE;
            allocate(&mut range, offset)?;
        }

        for (info, cfg) in info.iter_mut().zip(cfg) {
            // FIXME: actually do we care? I really don't know.
            if !cfg.slot_entry_size.is_power_of_two() {
                return Err(ServerError::EntrySizeError)?;
            }

            if cfg.ring_size % cfg.slot_entry_size != 0 {
                return Err(ServerError::RingSizeError)?;
            }

            if !(cfg.ring_size / cfg.slot_entry_size).is_power_of_two() {
                return Err(ServerError::RingCountError)?;
            }

            // One page for the ring's head.
            let offset_head = allocate(&mut range, Self::PAGE_SIZE)?;
            let offset_ring = allocate(
                &mut range,
                Self::page_requirement(cfg.ring_size)? * Self::PAGE_SIZE,
            )?;
            let offset_data = allocate(
                &mut range,
                Self::page_requirement(cfg.data_size)? * Self::PAGE_SIZE,
            )?;

            let lhs = if cfg.lhs < 0 { cfg.lhs } else { 0i32 };
            let rhs = if cfg.rhs < 0 { cfg.rhs } else { 0i32 };

            *info.version.get_mut() = 1;
            info.offset_head = data::ShOffset(offset_head);
            info.offset_ring = data::ShOffset(offset_ring);
            info.offset_data = data::ShOffset(offset_data);
            info.size_head = Self::PAGE_SIZE;
            info.size_ring = cfg.ring_size;
            info.size_data = cfg.data_size;
            info.size_slot_entry = cfg.slot_entry_size;

            if let Some(id) = data::RingIdentifier::new(lhs) {
                info.lhs = data::ClientSlot::for_advertisement(id, 0);
            }

            if let Some(id) = data::RingIdentifier::new(rhs) {
                info.rhs = data::ClientSlot::for_advertisement(id, 0);
            }
        }

        Ok(())
    }

    fn page_requirement<N: TryInto<u64>>(len: N) -> Result<u64, ServerError> {
        len.try_into()
            .map_or(Err(ServerError::SizeError), |len: u64| {
                let full = len / Self::PAGE_SIZE;
                let off = if len % Self::PAGE_SIZE == 0 { 0 } else { 1 };
                Ok(off + full)
            })
    }
}

impl ServerTask {
    fn track_client(&mut self, client: data::ClientIdentifier, idx: usize, side: data::ClientSide) {
        let entry = match self.tracker.entry(client) {
            hash_map::Entry::Occupied(entry) => entry.into_mut(),
            hash_map::Entry::Vacant(entry) => {
                // We're not going to track this client if we can't open its pid. Likely already
                // recycled.
                let Ok(pid) = client.open_pid() else {
                    return;
                };

                entry.insert(ClientTrackingData {
                    pid,
                    rings: HashSet::new(),
                })
            }
        };

        entry.rings.insert((idx, side));
    }
}

impl RingVersion {
    pub const fn new() -> Self {
        RingVersion(1)
    }
}

impl Default for RingVersion {
    fn default() -> Self {
        Self::new()
    }
}
