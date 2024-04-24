use crate::data;
use core::sync::atomic;

type ClientIdInt = u64;

#[allow(dead_code)] // Just for ownership.
pub struct OwnedFd(uapi::OwnedFd);

const _: () = {
    // This pre-condition is required so that all PID values can be utilized as ring identifiers.
    // The assertion here serves as a warning: we *can* provide facilities to make construction
    // fallible, but doing so might be much more tedious.
    assert!(
        core::mem::size_of::<uapi::c::pid_t>() == 4,
        "The platform you're using can not guarantee that PIDs can atomically replace slots in ring
        registration. Please open a pull-request for fallible interfaces instead."
    );
};

impl data::ClientIdentifier {
    #[cfg(feature = "uapi")]
    pub fn new() -> Self {
        let pid = uapi::getpid();
        assert!(pid > 0, "Unsupported pid {pid} reported from the OS");
        // It's necessary that any change to a PID results in a change in the low 32-bits, the size
        // of the futex we may be using to efficiently await this change.
        data::ClientIdentifier(pid as ClientIdInt)
    }

    #[cfg(feature = "uapi")]
    pub(crate) fn pid(self) -> uapi::c::pid_t {
        // This may erase 'upper bits', but that is by design! Those may be used for tagging by the
        // client, although we haven't decided how to provide such an API to control the padding in
        // a way that isn't so platform-dependent that it'd be unusable..
        self.0 as uapi::c::pid_t
    }

    #[cfg(feature = "uapi")]
    pub(crate) fn open_pid(self) -> Result<OwnedFd, uapi::Errno> {
        uapi::pidfd_open(self.pid(), uapi::c::PIDFD_NONBLOCK).map(OwnedFd)
    }
}

#[repr(C)]
pub struct FutexWaitv {
    /// Linux is prepared for support for 64-bit futexes. Only use 32-bit.
    pub val: u64,
    pub addr: u64,
    pub flags: u32,
    __reserved: u32,
}

#[cfg(feature = "uapi")]
pub fn futex_waitv(waiters: &[FutexWaitv]) {}

static __HIDDEN: atomic::AtomicU32 = atomic::AtomicU32::new(0);

impl FutexWaitv {
    /// A futex waiter that always wakes immediately (with EAGAIN).
    pub fn ready() -> Self {
        FutexWaitv {
            val: 1,
            addr: &__HIDDEN as *const _ as u64,
            flags: 0x02,
            __reserved: 0,
        }
    }

    /// A futex that never resolves.
    pub fn pending() -> Self {
        FutexWaitv {
            val: 0,
            addr: &__HIDDEN as *const _ as u64,
            flags: 0x02,
            __reserved: 0,
        }
    }

    pub fn from_u32(atomic: &atomic::AtomicU32, val: u32) -> Self {
        FutexWaitv {
            // Pad to 64-bit
            val: val.into(),
            addr: &atomic as *const _ as u64,
            flags: 0x02,
            __reserved: 0,
        }
    }

    pub const ATOMIC_U32: u32 = 0x02;
}
