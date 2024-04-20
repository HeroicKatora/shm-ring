use alloc::sync::{Arc, Weak};
use core::sync::atomic;
use core::{cell::UnsafeCell, mem, ptr};

use crate::{client, data, server};

use memmap2::MmapRaw;

pub unsafe trait RetainedMemory {
    fn data_from_head(&self) -> *mut [u8];
}

#[derive(Clone, Copy)]
struct SharedHead {
    #[allow(dead_code)]
    all: &'static UnsafeCell<[u8]>,
    aligned_tail: &'static UnsafeCell<[u8]>,
    ring: *mut data::RingHead,
}

#[derive(Clone)]
pub struct Shared {
    head: SharedHead,
    _retain: Arc<dyn RetainedMemory>,
}

impl Shared {
    pub fn new(shared: MmapRaw) -> Option<Self> {
        Self::with_retained(Arc::new(shared))
    }

    pub fn with_retained(val: Arc<dyn RetainedMemory>) -> Option<Self> {
        Some(Shared {
            head: unsafe { val.clone().to_head()? },
            _retain: val,
        })
    }

    /// Initialize the mapping as a server.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the mapping is not concurrently accessed, i.e. this value
    /// *owns* the memory. This applies to this process as well as other processes. Any other
    /// existing `Server` or `Client` instance sharing the same mapping is assumed an access. The
    /// server takes ownership of the mapping.
    ///
    /// We can't relax this into a static or ref-counted check due to external processes, hence it
    /// is an `unsafe` function. You should adhere to this constraint by creating the shared memory
    /// as a private mapped shm file and then later linking it into the file system.
    pub unsafe fn into_server(
        &self,
        cfg: server::ServerConfig,
    ) -> Result<server::Server, server::ServerError> {
        // Safety: exactly fulfills the preconditions, ours by definition.
        unsafe { server::Server::new(self.clone(), cfg) }
    }

    /// Access this ring as a client.
    ///
    /// This does not join any specific ring, nor precludes the process from _also_ acting as a
    /// server. Merely it checks if the server structure exists and can be utilized, as well as
    /// resolve the addresses with a consistency check with the underlying mapped memory.
    pub fn into_client(&self) -> Result<client::Client, client::ClientError> {
        // Safety: exactly fulfills the preconditions, ours by definition.
        unsafe { client::Client::new(self.clone()) }
    }

    /// Get the memory of the file, which is after the head page.
    pub fn tail(&self) -> *const UnsafeCell<[u8]> {
        self.head.aligned_tail as *const UnsafeCell<[u8]>
    }

    pub(crate) unsafe fn read_head(&self) -> Option<*const data::RingHead> {
        let ptr = self.head.ring;
        let magic = (*(ptr as *const atomic::AtomicU64)).load(atomic::Ordering::Relaxed);

        if !data::RingMagic(magic).test() {
            None
        } else {
            // Strengthen the load into an acquire, on success. This ensures all the other struct
            // members and their access synchronize-with the magic, which has also been written as
            // a release.
            atomic::fence(atomic::Ordering::Acquire);
            Some(ptr)
        }
    }

    pub(crate) unsafe fn init(
        &self,
        mut head: data::RingHead,
    ) -> (*const data::RingHead, Weak<dyn RetainedMemory>) {
        assert_eq!(Arc::weak_count(&self._retain), 0);
        let ownership = Arc::downgrade(&self._retain);
        let ptr = self.head.ring;

        // To ensure and _ordering_ relation, we write this last and as an atomic with release
        // ordering such that the volatile write of previous struct members is before for any
        // reader that also inspects the magic with an atomic acquire load.
        let magic = core::mem::replace(&mut head.ring_magic.0, 0);

        // Safety: as promised we currently own this address.
        unsafe { ptr::write_volatile(ptr, head) }
        (*(ptr as *const atomic::AtomicU64)).store(magic, atomic::Ordering::Release);

        (self.head.ring, ownership)
    }
}

unsafe impl RetainedMemory for MmapRaw {
    fn data_from_head(&self) -> *mut [u8] {
        let ptr = self.as_mut_ptr();
        let len = self.len();
        ptr::slice_from_raw_parts_mut(ptr, len)
    }
}

impl dyn RetainedMemory {
    /// Safety: requires that the caller keep-alive the memory until the return is dropped itself.
    unsafe fn to_head(self: Arc<Self>) -> Option<SharedHead> {
        let data = self.data_from_head();
        let data = unsafe { &*(data as *mut UnsafeCell<[u8]>) };
        let size = mem::size_of_val(data);

        if size < mem::size_of::<data::RingHead>() {
            return None;
        }

        if (data.get() as *const u8 as usize) % mem::align_of::<data::RingHead>() != 0 {
            return None;
        }

        // Safety: lifetime enlarged by caller, otherwise this is a no-op.
        let ring = data.get() as *mut data::RingHead;
        let tail = (data.get() as *const u8).add(mem::size_of::<data::RingHead>());
        let tail_size = size - mem::size_of::<data::RingHead>();

        let offset = tail.align_offset(mem::align_of::<data::RingHead>());

        if offset > tail_size {
            return None;
        }

        let aligned_tail = tail.add(offset);
        let aligned_size = tail_size - offset;

        let aligned_tail = ptr::slice_from_raw_parts(aligned_tail, aligned_size);
        let aligned_tail = unsafe { &*(aligned_tail as *const UnsafeCell<[u8]>) };

        Some(SharedHead {
            all: data,
            ring,
            aligned_tail,
        })
    }
}
