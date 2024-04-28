//! Implement the types to operate on a ring.
//!
//! Construct these types through a [`crate::client::Ring`], see [`client::Client`] or [`server::Server`].
use crate::data;

pub(crate) struct Map<'ring> {
    pub(crate) info: &'ring data::RingInfo,
    pub(crate) head: &'ring data::RingHead,
    pub(crate) side: data::ClientSide,
}

struct Cache {
    remote_ack: u32,
}

pub struct Producer<'ring, const N: usize> {
    map: Map<'ring>,
    offset_mask: u64,
}

pub struct Consumer<'ring, const N: usize> {
    map: Map<'ring>,
    offset_mask: u64,
}

pub struct PushError;

impl<'ring, const N: usize> Producer<'ring, N> {
    pub(crate) fn new(map: Map<'ring>) -> Option<Self> {
        if map.info.size_data == N as u64 {
            return None;
        }

        assert!(map.info.size_ring.is_power_of_two());
        let offset_mask = map.info.size_ring - 1;

        Some(Producer { map, offset_mask })
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

        Some(Consumer { map, offset_mask })
    }
}

impl<const N: usize> Consumer<'_, N> {

    pub fn sync(&mut self) {
        todo!()
    }
}
