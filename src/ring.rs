//! Implement the types to operate on a ring.
//!
//! Construct these types through a [`crate::client::Ring`], see [`client::Client`] or [`server::Server`].
use core::{cell::UnsafeCell, ptr, sync::atomic};

use crate::data;

/// Crate internal interface for constructing producer and consumers.
///
/// For simplicity we always send the whole ring's informations, and have the component itself
/// figure out which of these pointers to utilize. In particular, this is always borrowed
/// data and such can be mapped down to the right references.
pub(crate) struct Map<'ring> {
    pub(crate) info: &'ring data::RingInfo,
    pub(crate) head: &'ring data::RingHead,
    pub(crate) side: data::ClientSide,
    pub(crate) ring: &'ring UnsafeCell<[u8]>,
}

struct Cache {
    remote_ack: u32,
}

pub struct Producer<'ring, const N: usize> {
    /// The slot of the local head to indicate how many are filled.
    head: &'ring atomic::AtomicU32,
    /// The slot of the remote to indicate what has been read.
    tail: &'ring atomic::AtomicU32,
    ring: &'ring [UnsafeCell<[u8; N]>],
    offset_mask: u64,
    cached_head: u32,
    cached_tail: u32,
}

pub struct Consumer<'ring, const N: usize> {
    /// The slot of the remote to indicate what has been read.
    tail: &'ring atomic::AtomicU32,
    /// The slot of the local head to indicate how many are filled.
    head: &'ring atomic::AtomicU32,
    ring: &'ring [UnsafeCell<[u8; N]>],
    offset_mask: u64,
    cached_head: u32,
    cached_tail: u32,
}

pub struct PushError;

impl<'ring, const N: usize> Producer<'ring, N> {
    pub(crate) fn new(map: Map<'ring>) -> Option<Self> {
        if map.info.size_data == N as u64 {
            return None;
        }

        assert!(map.info.size_ring.is_power_of_two());
        let offset_mask = map.info.size_ring - 1;
        let ring = cell_as_arrays(map.ring);

        let head = map.head.select_producer(map.side);
        let tail = map.head.select_consumer(!map.side);

        let cached_head = head.load(atomic::Ordering::Relaxed);
        let cached_tail = head.load(atomic::Ordering::Acquire);

        Some(Producer {
            head,
            tail,
            offset_mask,
            ring,
            cached_head,
            cached_tail,
        })
    }
}

impl<const N: usize> Producer<'_, N> {
    pub fn push(&mut self, data: &[u8; N]) -> Result<(), PushError> {
        todo!()
    }

    pub fn push_many<I>(&mut self, iter: I) -> usize
    where
        I: Iterator<Item = [u8; N]>,
    {
        let mut n = 0;
        todo!();
        n
    }

    pub fn sync(&mut self) {
        todo!()
    }
}

impl<'ring, const N: usize> Consumer<'ring, N> {
    pub(crate) fn new(map: Map<'ring>) -> Option<Self> {
        if map.info.size_data == N as u64 {
            return None;
        }

        assert!(map.info.size_ring.is_power_of_two());
        let offset_mask = map.info.size_ring - 1;
        let ring = cell_as_arrays(map.ring);

        let head = map.head.select_producer(!map.side);
        let tail = map.head.select_consumer(map.side);

        let cached_head = head.load(atomic::Ordering::Relaxed);
        let cached_tail = head.load(atomic::Ordering::Acquire);

        Some(Consumer {
            head,
            tail,
            offset_mask,
            ring,
            cached_head,
            cached_tail,
        })
    }

    pub fn iter(&mut self) -> impl Iterator<Item = [u8; N]> {
        todo!();
        core::iter::empty()
    }
}

impl<const N: usize> Consumer<'_, N> {
    pub fn sync(&mut self) {
        todo!()
    }
}

fn cell_as_arrays<const N: usize>(cell: &UnsafeCell<[u8]>) -> &'_ [UnsafeCell<[u8; N]>] {
    let count = core::mem::size_of_val(cell) / N;
    let ring_data: *mut [u8] = cell.get();
    unsafe { &*ptr::slice_from_raw_parts(ring_data as *const UnsafeCell<[u8; N]>, count) }
}
