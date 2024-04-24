use crate::data;
use core::{marker::PhantomData, sync::atomic, time::Duration};

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
pub struct FutexWaitv<'lt> {
    /// Linux is prepared for support for 64-bit futexes. Only use 32-bit.
    pub val: u64,
    pub addr: u64,
    _addr_owner: PhantomData<&'lt atomic::AtomicU32>,
    pub flags: u32,
    __reserved: u32,
}

unsafe fn _sys_futex_waitv(
    waiters: *mut FutexWaitv,
    nr: uapi::c::c_int,
    flags: uapi::c::c_int,
    timeout: Option<&mut uapi::c::timespec>,
    clock_source: uapi::c::clockid_t,
) -> i64 {
    uapi::c::syscall(
        uapi::c::SYS_futex_waitv,
        waiters,
        nr,
        flags,
        timeout,
        clock_source,
    )
}

#[cfg(feature = "uapi")]
pub fn futex_waitv(waiters: &mut [FutexWaitv], timeout: Duration) -> i64 {
    let nr = waiters.len().try_into().unwrap_or(uapi::c::c_int::MAX);

    let mut timespec = uapi::c::timespec {
        tv_nsec: timeout.subsec_nanos().into(),
        tv_sec: timeout.as_secs() as uapi::c::time_t,
    };

    unsafe {
        _sys_futex_waitv(
            waiters as *mut _ as *mut _,
            nr,
            0,
            Some(&mut timespec),
            uapi::c::CLOCK_MONOTONIC,
        )
    }
}

static __HIDDEN: atomic::AtomicU32 = atomic::AtomicU32::new(0);

impl FutexWaitv<'static> {
    /// A futex waiter that always wakes immediately (with EAGAIN).
    pub fn ready() -> Self {
        FutexWaitv {
            val: 1,
            addr: &__HIDDEN as *const _ as u64,
            _addr_owner: PhantomData,
            flags: FutexWaitv::ATOMIC_U32,
            __reserved: 0,
        }
    }

    /// A futex that never resolves.
    pub fn pending() -> Self {
        FutexWaitv {
            val: 0,
            addr: &__HIDDEN as *const _ as u64,
            _addr_owner: PhantomData,
            flags: FutexWaitv::ATOMIC_U32,
            __reserved: 0,
        }
    }

    pub const ATOMIC_U32: u32 = 0x02;

    pub const EAGAIN: i64 = -::uapi::c::EAGAIN as i64;
    pub const ETIMEDOUT: i64 = -::uapi::c::ETIMEDOUT as i64;
    pub const ERESTARTSYS: i64 = -::uapi::c::ERESTART as i64;
}

impl<'word> FutexWaitv<'word> {
    pub fn from_u32(atomic: &'word atomic::AtomicU32, val: u32) -> Self {
        FutexWaitv {
            // Pad to 64-bit
            val: val.into(),
            addr: &atomic as *const _ as u64,
            _addr_owner: PhantomData,
            flags: FutexWaitv::ATOMIC_U32,
            __reserved: 0,
        }
    }
}
