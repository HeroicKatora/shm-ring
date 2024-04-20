//! Defines the central data structures.
use core::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};

#[repr(C)]
pub struct RingHead {
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
#[repr(transparent)]
pub struct ClientSlot(pub AtomicI64);

/// An offset within into the head structure of the ring, from the ring info struct (by
/// convention that is the start of the shared memory file).
#[repr(transparent)]
pub struct ShOffset(pub u64);

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
    pub _padding0: [u64; 3],
    // Here we are at 12 · 8 byte
    pub rhs: ClientSlot,
    pub _padding1: [u64; 3],
    // Here we are at 16 · 8 byte
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct ClientIdentifier(pub(crate) u64);

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct RingIdentifier(pub(crate) i64);

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

impl ClientSlot {
    /// Atomically exchange the slot with a request to join with a specific client.
    pub fn insert(&self, client: ClientIdentifier) -> Result<RingIdentifier, ClientIdentifier> {
        let client = client.0 as i64;
        assert!(client > 0, "Invalid client ID");

        // FIXME: this is a problem if we have heavy contention ABA to a ring. Luckily, we assume
        // that this is not the case.. However, maybe we shouldn't spin-lock forever on this?
        let acquisition = self
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                Some(client).filter(|_| n <= 0)
            });

        match acquisition {
            Ok(id) => Ok(RingIdentifier(id)),
            Err(id) => {
                debug_assert!(id > 0);
                Err(ClientIdentifier(id as u64))
            }
        }
    }

    pub fn leave(&self, id: ClientIdentifier) -> Result<(), i64> {
        self.0
            .compare_exchange_weak(
                id.0 as i64,
                RingIdentifier::default().0,
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .map(|_| ())
    }
}

pub(crate) fn align_offset<U>(ptr: *const u8) -> usize {
    let addr = ptr as usize;
    addr.wrapping_neg() % core::mem::align_of::<U>()
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
