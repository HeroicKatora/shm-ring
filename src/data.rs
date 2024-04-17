//! Defines the central data structures.
use core::sync::atomic::{AtomicU64, AtomicU32};

#[repr(C)]
pub struct RingHead {
    pub ring_magic: RingMagic,
    pub ring_count: u64,
    pub ring_ping: RingPing,
}

#[derive(Default)]
pub struct RingPing {
    pub ring_ping: RingClientPing,
    pub ring_pong: RingServerPong,
}

#[repr(transparent)]
pub struct RingMagic(u64);

#[repr(C, align(4096))]
pub struct Rings([RingInfo]);

/// A 'register' with which clients can ping the server into action, by incrementing. We have a
/// futex waiting on it.
#[derive(Default)]
#[repr(transparent)]
pub struct RingClientPing(AtomicU32);

/// A 'register' operated by the server, which acknowledges clients pings.
#[derive(Default)]
#[repr(transparent)]
pub struct RingServerPong(AtomicU32);

/// A slot with which a client can register to a ring..
#[repr(transparent)]
pub struct ClientSlot(AtomicU64);

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
    pub version: u64,
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

impl RingMagic {
    const MAGIC: u64 = 0x9e6c_a4fd8624a738;

    pub fn new() -> Self {
        RingMagic(Self::MAGIC)
    }

    pub fn test(&self) -> bool {
        self.0 == Self::MAGIC
    }
}
