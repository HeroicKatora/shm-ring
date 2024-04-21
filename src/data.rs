//! Defines the central data structures.
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use core::{alloc, cell::UnsafeCell, ops};

use linux_futex::{op as futop, AsFutex, Futex, Shared};

#[repr(C)]
pub struct ShmHead {
    pub ring_magic: RingMagic,
    pub ring_count: u64,
    pub ring_offset: u64,
    pub ring_ping: RingPing,
}

#[derive(Default)]
pub struct RingPing {
    pub ring_ping: RingClientPing,
    pub ring_pong: RingServerPong,
}

#[repr(transparent)]
pub struct RingMagic(pub(crate) u64);

#[derive(Copy, Clone, PartialEq, Eq)]
pub struct RingIndex(pub usize);

#[repr(C, align(4096))]
pub struct Rings([RingInfo]);

/// A 'register' with which clients can ping the server into action, by incrementing. We have a
/// futex waiting on it.
#[derive(Default)]
#[repr(transparent)]
pub struct RingClientPing(pub AtomicU32);

/// A 'register' operated by the server, which acknowledges clients pings.
#[derive(Default)]
#[repr(transparent)]
pub struct RingServerPong(pub AtomicU32);

/// A slot with which a client can register to a ring..
#[repr(C, align(64))]
pub struct ClientSlot {
    /// The current owner of this slot. That is:
    /// - a positive value, always a PID, if the slot is owned by a process.
    /// - `0` if the slot is owned by the coordination authority.
    /// - a negative value if the slot is available, advertising some tag.
    pub owner: AtomicU32,
    /// An additional info advertised by the owner. Only the owner should write here.
    pub tag: AtomicU32,
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

/// An offset within into the head structure of the ring, from the ring info struct (by
/// convention that is the start of the shared memory file).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ShOffset(pub u64);

/// Number of U32 values to pad, to avoid cache interference between atomics.
const ANTI_INTERFERENCE_PADDING_U32: usize = 31;

/// Published by the server, information on the ring and a slot for registering as a client to the
/// ring via an atomic CAS.
#[repr(C)]
pub struct RingInfo {
    /// Always `1` when this ring is active. Otherwise, `0`.
    ///
    /// NOTE: Maybe this could be used by the server to deactivate a ring while fiddling with its
    /// internals, only when no client is assigned. But how to correctly order the checks in other
    /// fields seems complicated. So this is basically informational.
    pub version: AtomicU64,
    /// The offset at which to find this rings head structure.
    pub offset_head: ShOffset,
    /// The offset at which to find this rings slot structure.
    pub offset_ring: ShOffset,
    /// The offset at which to find this rings data structure.
    pub offset_data: ShOffset,
    /// The byte size of that rings head, should be checked for compatibility.
    pub size_head: u64,
    /// The byte size of that rings slot structure.
    pub size_ring: u64,
    /// The byte size of that rings data structure.
    pub size_data: u64,
    /// The byte size of each entry in the rings slot structure.
    pub size_slot_entry: u64,
    // Here we are at 8 · 8 byte.
    pub lhs: ClientSlot,
    pub _padding0: NoAccess<UnsafeCell<[u64; ANTI_INTERFERENCE_PADDING_U32]>>,
    // Here we are at 12 · 8 byte
    pub rhs: ClientSlot,
    pub _padding1: NoAccess<UnsafeCell<[u64; ANTI_INTERFERENCE_PADDING_U32]>>,
    // Here we are at 16 · 8 byte
}

#[repr(C)]
pub struct RingHead {
    pub lhs: RingHeadHalf,
    pub rhs: RingHeadHalf,
}

#[repr(C)]
pub struct RingHeadHalf {
    pub producer: AtomicU32,
    pub _padding0: NoAccess<UnsafeCell<[u32; ANTI_INTERFERENCE_PADDING_U32]>>,
    pub consumer: AtomicU32,
    pub _padding1: NoAccess<UnsafeCell<[u32; ANTI_INTERFERENCE_PADDING_U32]>>,
}

/// Wraps memory, not allowing *any* access.
///
/// This allows containers with such fields (for padding) to be `Sync`.
#[repr(transparent)]
pub struct NoAccess<T>(T);

// Safety: no `&T` can even be created, so this is always sound.
unsafe impl<T> Sync for NoAccess<T> {}
// Safety: `Copy` ensures that the value, if any, is completely inert. Since no reference, nor
// owned value, to it can be created after wrapping it in `NoAccess` there can be no code relying
// on any invariants that are broken by viewing the bytes in a different thread.
unsafe impl<T: Copy> Send for NoAccess<T> {}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ClientIdentifier(pub(crate) u64);

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct RingIdentifier(pub(crate) i32);

impl RingMagic {
    const MAGIC: u64 = 0x9e6c_a4fd8624a738;

    pub fn new() -> Self {
        RingMagic(Self::MAGIC)
    }

    pub fn test(&self) -> bool {
        self.0 == Self::MAGIC
    }
}

impl Rings {
    pub fn get(&self, RingIndex(idx): RingIndex) -> Option<&RingInfo> {
        self.0.get(idx as usize)
    }
}

impl<'lt> IntoIterator for &'lt Rings {
    type Item = &'lt RingInfo;
    type IntoIter = core::slice::Iter<'lt, RingInfo>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
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

impl ClientIdentifier {
    pub fn to_slot_id(self) -> u32 {
        let client = self.0 as i32;
        // As promised by the constructor in `uapi.rs`
        assert!(client > 0, "Invalid client ID");
        client as u32
    }
}

impl RingIdentifier {
    pub fn to_slot_id(self) -> u32 {
        self.0 as u32
    }
}

impl ClientSlot {
    pub(crate) fn for_advertisement(owner: i32, tag: u32) -> Self {
        assert!(owner < 0);

        ClientSlot {
            owner: (owner as u32).into(),
            tag: tag.into(),
        }
    }

    /// Atomically exchange the slot with a request to join with a specific client.
    pub fn insert(&self, client: ClientIdentifier) -> Result<RingIdentifier, ClientIdentifier> {
        let client = client.0 as i32;
        assert!(client > 0, "Invalid client ID");
        let client = client as u32;

        // FIXME: this is a problem if we have heavy contention ABA to a ring. Luckily, we assume
        // that this is not the case.. However, maybe we shouldn't spin-lock forever on this?
        let acquisition =
            self.owner
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n: u32| {
                    Some(client).filter(|_| n as i32 <= 0)
                });

        match acquisition {
            Ok(id) => Ok(RingIdentifier(id as i32)),
            Err(id) => {
                debug_assert!(id > 0);
                Err(ClientIdentifier(id as u64))
            }
        }
    }

    pub fn leave(&self, id: ClientIdentifier) -> Result<(), u32> {
        self.owner
            .compare_exchange_weak(
                id.to_slot_id(),
                RingIdentifier::default().to_slot_id(),
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .map(|_| ())
    }

    pub fn inspect(&self) -> Result<ClientIdentifier, RingIdentifier> {
        let id: u32 = self.owner.load(Ordering::Relaxed);
        let id = id as i32;

        if id > 0 {
            Ok(ClientIdentifier(id as u64))
        } else {
            Err(RingIdentifier(id as i32))
        }
    }
}

impl RingInfo {
    /// Leave a ring, as an owner of a side.
    ///
    /// This will atomically swap the client slot for `0` and wake any futex waiting on the head of
    /// the queue of the side being left, if any.
    pub fn leave_as_owner_with_futex(&self, side: ClientSide, head: &RingHead) {
        let slot: &Futex<Shared> = self.select_slot(side).owner.as_futex();
        let head: &Futex<Shared> = head.select_producer(side).as_futex();

        // Effectively: always assign, always wake since the current value _must_ be our own PID if
        // used correctly. We only really use `> 0` as dummy for bad usage.
        let op = futop::Op::assign(0) + futop::Cmp::ge(0);
        head.wake_op(i32::MAX, slot, op, i32::MAX);
    }

    pub fn select_slot(&self, side: ClientSide) -> &ClientSlot {
        match side {
            ClientSide::Left => &self.lhs,
            ClientSide::Right => &self.rhs,
        }
    }
}

impl RingHead {
    pub fn select_producer(&self, side: ClientSide) -> &AtomicU32 {
        match side {
            ClientSide::Left => &self.lhs.producer,
            ClientSide::Right => &self.rhs.producer,
        }
    }
}

pub(crate) fn align_offset<U>(ptr: *const u8) -> usize {
    align_offset_val(ptr, alloc::Layout::new::<U>())
}

pub(crate) fn align_offset_val(ptr: *const u8, layout: alloc::Layout) -> usize {
    let addr = ptr as usize;
    addr.wrapping_neg() % layout.align()
}

#[test]
fn align_offset_is_correct() {
    assert_eq!(align_offset::<u64>(0usize as *const u8), 0);
    assert_eq!(align_offset::<u64>(1usize as *const u8), 7);
    assert_eq!(align_offset::<u64>(2usize as *const u8), 6);
    assert_eq!(align_offset::<u64>(3usize as *const u8), 5);
    assert_eq!(align_offset::<u64>(4usize as *const u8), 4);
    assert_eq!(align_offset::<u64>(7usize as *const u8), 1);
    assert_eq!(align_offset::<u64>(8usize as *const u8), 0);
}
