use core::{cell, mem, ptr};
use alloc::sync::Weak;

use crate::{data, frame};

pub struct Server {
    ring: frame::Shared,
    server: ServerHead,
    owner: Weak<dyn frame::RetainedMemory>,
}

#[derive(Clone, Copy)]
struct ServerHead {
    /// The frozen head.
    pub head: &'static data::RingHead,
    pub info: &'static data::Rings,
    pub data: &'static cell::UnsafeCell<[u8]>,
}

pub struct ServerConfig<'lt> {
    pub vec: &'lt [RingConfig],
}

#[derive(Clone, Copy)]
pub struct RingVersion(u64);

pub enum ServerError {
    TooSmall,
    SizeError,
}

pub struct RingConfig {
    pub version: RingVersion,
    pub ring_size: u64,
    pub data_size: u64,
    pub slot_entry_size: u64,
}

impl Server {
    const PAGE_SIZE: u64 = 4096;

    /// Initialize a server, if the configuration checks out.
    ///
    /// # Safety
    ///
    /// Must be called by [`frame::Shared::into_server`], using all its precondition.
    pub(crate) unsafe fn new(ring: frame::Shared, cfg: ServerConfig) -> Result<Self, ServerError> {
        let info_tail = ring.tail();
        // Safety: we store the shared memory.
        let info_tail = unsafe { &*info_tail };
        let size = mem::size_of_val(info_tail);

        let rings_len = cfg.vec.len() * mem::size_of::<data::RingInfo>();
        let pages_head = Self::page_requirement(rings_len)?;
        let rings_len = (pages_head * Self::PAGE_SIZE) as usize;

        let info = info_tail.get() as *const data::RingInfo;
        let info = ptr::slice_from_raw_parts(info, cfg.vec.len());

        let tail = unsafe { (info_tail.get() as *const u8).add(rings_len) };
        let tail_size = size - rings_len;

        let tail = ptr::slice_from_raw_parts(tail, tail_size);
        let head = data::RingHead {
            ring_magic: data::RingMagic::new(),
            ring_count: cfg.vec.len() as u64,
            ring_ping: data::RingPing::default(),
        };

        // Safety: we are known to be the only owner, since this `new` is only called when the
        // caller has already promised it.
        let (head, owner) = unsafe { ring.init(head) };

        // Safety: all properly initialized at this point.
        let server = ServerHead {
            head: unsafe { &*head },
            info: unsafe { &*(info as *const data::Rings) },
            data: unsafe { &*(tail as *const cell::UnsafeCell<[u8]>) },
        };

        Ok(Server { ring, server, owner })
    }

    fn page_requirement(len: usize) -> Result<u64, ServerError> {
        len.try_into()
            .map_or(Err(ServerError::SizeError), |len: u64| {
                Ok(len / Self::PAGE_SIZE)
            })
    }
}
