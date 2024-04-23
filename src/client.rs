use core::{alloc, mem, ptr, sync::atomic, time};

use linux_futex::{AsFutex, Futex, Shared};

use crate::{data, frame};

pub struct Client {
    ring: frame::Shared,
    head: ClientHead,
}

pub struct Ring {
    ring: frame::Shared,
    map: RingMap,
    /// The ID which we acquired the slot from.
    ring_id: data::RingIdentifier,
}

pub struct RingRequest {
    pub index: data::RingIndex,
    pub side: data::ClientSide,
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
    BadRingOffsetHead,
    BadRingOffsetData,
    BadRingOffsetRing,
    Unsupported,
    Taken(data::ClientIdentifier),
}

#[derive(Clone, Copy)]
struct ClientHead {
    /// The frozen head.
    pub head: &'static data::ShmHead,
    // FIXME: not necessarily correct. We only know if a ring is `Rings` if they are V1.
    pub rings: &'static data::Rings,
    // FIXME: missing tail for data pages, and that tail's total offset.
}

struct OwnedRingSlot {
    info: &'static data::RingInfo,
    head: &'static data::RingHead,
    side: data::ClientSide,
    identity: data::ClientIdentifier,
}

struct RingMap {
    head: &'static data::ShmHead,
    ring_slot: OwnedRingSlot,
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

        let head = ring
            .get_aligned_data_at_offset::<data::RingHead>(ring_info.offset_head)
            .ok_or(RingJoinError::BadRingOffsetHead)?;

        let ring_layout = Self::spec_as_layout(ring_info.size_ring, ring_info.size_slot_entry)
            .ok_or(RingJoinError::BadRingOffsetRing)?;
        let _ = ring
            .get_aligned_data_at_offset_val(ring_info.offset_ring, ring_layout)
            .ok_or(RingJoinError::BadRingOffsetRing)?;

        let data_layout = Self::spec_as_layout(ring_info.size_data, 1u64)
            .ok_or(RingJoinError::BadRingOffsetRing)?;
        let _ = ring
            .get_aligned_data_at_offset_val(ring_info.offset_data, data_layout)
            .ok_or(RingJoinError::BadRingOffsetRing)?;

        let slot = ring_info.select_slot(req.side);
        let ring_id = slot.insert(req.tid).map_err(RingJoinError::Taken)?;

        // Immediately afterwards we are responsible for that region.
        let owned_ring = OwnedRingSlot {
            head: unsafe { &*head },
            identity: req.tid,
            info: ring_info,
            side: req.side,
        };

        let frame = RingMap {
            head: self.head.head,
            ring_slot: owned_ring,
        };

        Ok(Ring {
            ring,
            map: frame,
            ring_id,
        })
    }

    fn spec_as_layout(size: u64, align: u64) -> Option<alloc::Layout> {
        let size: usize = size.try_into().ok()?;
        let align: usize = align.try_into().ok()?;
        alloc::Layout::from_size_align(size, align).ok()
    }
}

impl Ring {
    /// Wait on the other side to move the head and wake us.
    ///
    /// Alternatively, the operation may be woken by the coordination authority if the PID
    /// controlling the other half of this ring dropped out abnormally. Returns `true` if the wait
    /// was successfully completed by a `wake`. Returns `false` if the other half already left the
    /// ring, leaves the ring normally while waiting, is reaped by the authority, or a timeout
    /// occurs.
    pub fn block_on_message(&self, timeout: time::Duration) -> bool {
        // Does a bit of a weird dance.. We wait on the _slot_ and not the head counter. The reason
        // here is that it is the _slot_ which must stay constant primarily as one must only wait
        // while there is an active PID on the other side. We rely on the other side to requeue us
        // to the actual head before the notify waiters. Which is absolutely odd, but what can you
        // do.
        //
        // I'd like a `FUTEX_WAIT2` operation which enqueues us to a futex based on the atomic
        // comparison with another futex's value?
        //
        // But really it isn't critical whether the head's value is still as expected, as long as
        // the other half is alive it should periodically re-check even when it has woken on some
        // particular head value. Hence, we do not actually race with an update to the head? This
        // is very surprising..
        //
        // The only real alternative would be us spawning a separate thread wherein we can, for all
        // blocked heads, do a `FUTEX_WAKE_OP` on the producer, with the cached previously loaded
        // value, to wake our futexes blocked on the slot when the producer changed to simulate
        // parts of the effect of having checked two values.
        let block = &self.map.ring_slot.head.blocked.0;
        let owner = self.map.ring_slot.side.as_block_slot();

        let prior = block.compare_exchange_weak(
            0,
            owner,
            atomic::Ordering::Relaxed,
            atomic::Ordering::Relaxed,
        );

        // Do not wait if there is no other side anymore (or yet)..
        match prior {
            Ok(_) => {}
            // FIXME: return an error indicating we're shutting down, other side blocked indefinitely.
            Err(prior) if prior as i32 <= 0 => return false,
            // FIXME: return a different error, already blocked.
            Err(_prior) => return false,
        }

        let slot: &Futex<Shared> = block.as_futex();
        // FIXME: a dedicated slot might be better. Let each of the sides declare their intention
        // on whether messages are coming or blocked. And also have at most one side wait on actual
        // messages. Note that such state can be de-initialized just the same during leaving (and
        // also even unblocked by the authority while the slot is empty).
        //
        // This is going to be interesting as we need 3 (or at least two) checks.
        slot.wait_for(owner, timeout).is_ok()
    }

    pub fn wake(&self) -> u32 {
        self.map.ring_slot.wake()
    }
}

impl OwnedRingSlot {
    pub fn wake(&self) -> u32 {
        // See `Ring::wait` for an explanation of this dance.
        let slot = &self.head.blocked.0;
        let slot: &Futex<Shared> = slot.as_futex();

        let producer = self.head.select_producer(self.side);
        let producer: &Futex<Shared> = producer.as_futex();
        // First, requeue any waiters that assume we're leaving any second.
        //
        // FIXME: it's not entirely clear why we re-queue all waiters to that if we know the
        // comparison to succeed. We could _merge_ different waiters with this strategy but
        // currently no waiter is taking only the producer futex. In particular, some might wait on
        // a side beginning to produce again (see: fixme in `wait`) while others are signalling
        // when our side is blocked on incoming messages (which is an exclusive condition that only
        // one side must take at a time and which can be stolen when the whole ring is going to be
        // shut down).
        let owner = (!self.side).as_block_slot();
        let Ok(_n_waiters_moved) = slot.cmp_requeue(owner, 0, producer, i32::MAX) else {
            // Oh, the lock wasn't actually taken. Obviously if we'd have taken the lock then we
            // wouldn't be running in this. Right?
            return 0;
        };

        producer.wake(i32::MAX) as u32
    }
}

impl Drop for OwnedRingSlot {
    fn drop(&mut self) {
        self.info.leave_as_owner_with_futex(self.side, self.head);
    }
}
