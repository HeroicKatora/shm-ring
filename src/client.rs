use core::{cell, mem, ptr};

use crate::{data, frame};

pub struct Client {
    ring: frame::Shared,
    head: RingHead,
}

#[derive(Debug)]
pub enum ClientError {
    /// This isn't actually a ring at all..
    BadMagic,
    SizeError,
    TooSmall,
    UnalignedMap,
}

#[derive(Clone, Copy)]
struct RingHead {
    /// The frozen head.
    pub head: &'static data::RingHead,
    pub info: &'static data::Rings,
    // FIXME: missing tail for data pages, and that tail's total offset.
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

        let head = RingHead {
            head,
            info: unsafe { &*(rings as *const data::Rings) },
        };

        Ok(Client { ring, head })
    }
}
