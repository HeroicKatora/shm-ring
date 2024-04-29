//! Implement the types to operate on a ring.
//!
//! Construct these types through a [`crate::client::Ring`], see [`client::Client`] or [`server::Server`].
use core::{cell::UnsafeCell, ptr};

use crate::data;

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
    map: Map<'ring>,
    offset_mask: u64,
    ring: &'ring [UnsafeCell<[u8; N]>],
}

pub struct Consumer<'ring, const N: usize> {
    map: Map<'ring>,
    offset_mask: u64,
    ring: &'ring [UnsafeCell<[u8; N]>],
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

        Some(Producer {
            map,
            offset_mask,
            ring,
        })
    }
}

impl<const N: usize> Producer<'_, N> {
    pub fn push(&mut self, data: &[u8; N]) -> Result<(), PushError> {
        todo!()
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

        Some(Consumer {
            map,
            offset_mask,
            ring,
        })
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
