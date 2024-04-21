use core::{mem, ops, ptr, sync::atomic};

use crate::{data, frame};

pub struct Client {
    ring: frame::Shared,
    head: ClientHead,
}

pub struct Ring {
    ring: frame::Shared,
    frame: RingHead,
    /// The ID which we acquired the slot from.
    ring_id: data::RingIdentifier,
}

/// Identifies the side of the ring.
///
/// A ring is, from the high-level view, a connection between two equals. There is no ordering
/// relationship here. Of course, specific rings may disagree with that.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClientSide {
    Left,
    Right,
}

pub struct RingRequest {
    pub index: data::RingIndex,
    pub side: ClientSide,
    pub tid: data::ClientIdentifier,
}

#[derive(Debug)]
pub enum ClientError {
    /// This isn't actually a ring at all..
    BadMagic,
    SizeError,
    TooSmall,
    UnalignedMap,
}

#[derive(Debug)]
pub enum RingJoinError {
    BadRingIndex,
    Unsupported,
    Taken(data::ClientIdentifier),
}

#[derive(Clone, Copy)]
struct ClientHead {
    /// The frozen head.
    pub head: &'static data::RingHead,
    // FIXME: not necessarily correct. We only know if a ring is `Rings` if they are V1.
    pub rings: &'static data::Rings,
    // FIXME: missing tail for data pages, and that tail's total offset.
}

struct OwnedRingInfo {
    head: &'static data::RingInfo,
    side: ClientSide,
    identity: data::ClientIdentifier,
}

struct RingHead {
    head: &'static data::RingHead,
    info: OwnedRingInfo,
}

impl Client {
    pub(crate) unsafe fn new(ring: frame::Shared) -> Result<Self, ClientError> {
        let info_tail = ring.tail();
        // Safety: we store the shared memory.
        let info_tail = unsafe { &*info_tail };
        let size = mem::size_of_val(info_tail);

        let Some(head) = (unsafe { ring.read_head() }) else {
            return Err(ClientError::BadMagic);
        };

        // Safety: we store the shared memory.
        let head = unsafe { &*head };

        let ring_offset = head
            .ring_offset
            .try_into()
            .map_err(|_| ClientError::SizeError)?;

        let size = size.checked_sub(ring_offset).ok_or(ClientError::TooSmall)?;
        // Safety: size check we just did with checked_sub.
        let info_tail = unsafe { (info_tail.get() as *const u8).add(ring_offset) };

        if data::align_offset::<data::RingInfo>(info_tail) != 0 {
            return Err(ClientError::UnalignedMap);
        }

        let ring_max_count = size / mem::size_of::<data::RingInfo>();
        let max_rings = ring_max_count.try_into().unwrap_or(u64::MAX);

        if max_rings < head.ring_count {
            return Err(ClientError::TooSmall);
        }

        let ring_count = head.ring_count as usize;
        let rings = info_tail as *const data::RingInfo;
        // Safety: memory requirement smaller than `size`, we bailed out everywhere math is not an
        // under approximation of available space.
        let rings = ptr::slice_from_raw_parts(rings, ring_count);

        let head = ClientHead {
            head,
            rings: unsafe { &*(rings as *const data::Rings) },
        };

        Ok(Client { ring, head })
    }

    pub fn join(&self, req: &RingRequest) -> Result<Ring, RingJoinError> {
        // Protects any copy of `self.frame` we create.
        let ring = self.ring.clone();

        let ring_info = self
            .head
            .rings
            .get(req.index)
            .ok_or(RingJoinError::BadRingIndex)?;

        if ring_info.version.load(atomic::Ordering::Acquire) != 1 {
            return Err(RingJoinError::Unsupported);
        }

        let slot = req.side.select(&ring_info);
        let ring_id = slot.insert(req.tid).map_err(RingJoinError::Taken)?;

        // Immediately afterwards we are responsible for that region.
        let owned_ring = OwnedRingInfo {
            identity: req.tid,
            head: ring_info,
            side: req.side,
        };

        let frame = RingHead {
            head: self.head.head,
            info: owned_ring,
        };

        Ok(Ring {
            ring,
            frame,
            ring_id,
        })
    }
}

impl ClientSide {
    fn select(self, ring: &data::RingInfo) -> &data::ClientSlot {
        match self {
            ClientSide::Left => &ring.lhs,
            ClientSide::Right => &ring.rhs,
        }
    }
}

impl ops::Not for ClientSide {
    type Output = ClientSide;

    fn not(self) -> ClientSide {
        match self {
            ClientSide::Left => ClientSide::Right,
            ClientSide::Right => ClientSide::Left,
        }
    }
}

impl Ring {}

impl Drop for OwnedRingInfo {
    fn drop(&mut self) {
        let _ = self.side.select(&self.head).leave(self.identity);
    }
}
