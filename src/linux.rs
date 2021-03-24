use super::{ConnectError, ShmClient, ShmController};

use core::sync::atomic::{fence, Ordering};
use linux_futex::{AsFutex, Futex, Shared, WaitError};

// Enable futex based join.
pub fn join_methods() -> u32 {
    0x2
}

pub fn own_pid() -> u64 {
    unsafe { libc::getpid() as u64 }
}

pub fn futex(client: &ShmClient, this: u32) -> Result<u32, ConnectError> {
    assert!(this <= i32::MAX as u32, "Can't signal more than 0x7fff_ffff clients with futex");
    let open = client.open();
    let open = &open.futex;

    // Acquire the slot within the open queue
    while let Err(err) = open.compare_exchange(-1, this as i32, Ordering::AcqRel, Ordering::Acquire) {
        let futex: &Futex<Shared> = open.as_futex();
        // Shortly wait for this to clear. Never mind if it did already.
        //
        // Point (a), wait on entry to critical section.
        let _ = futex.wait_bitset(err, 0x1 << 31);
    }

    loop {
        // Wait for a reset with highest bit set, i.e. write of `-1` to the slot, i.e. reacquired
        // by the server itself after it added us.
        match (open.as_futex() as &Futex<Shared>).wait_bitset(this as i32, 0x1 << 31) {
            Ok(()) => {},
            // If someone else updated before we woke, we might have a wrong value but succeeded.
            Err(WaitError::WrongValue) => break,
            Err(WaitError::Interrupted) => {},
        }
    }

    // We've left the critical section. There may be more clients waiting at (a)
    (open.as_futex() as &Futex<Shared>).wake(i32::MAX);

    // The server writes the mutex with release, ensuring all state management completed. We will
    // need to acquire it to complete the transfer of the queue state to us.
    fence(Ordering::Acquire);

    // Queues are ID mapped for clients..
    Ok(this)
}

pub fn join_futex(server: &ShmController, transaction: impl FnOnce(u32)) {
    let join_futex = &server.open().futex;
    let request = join_futex.load(Ordering::Acquire) as u32;
    if request < server.monitor().max_members.load(Ordering::Relaxed) {
        assert!(request <= i32::MAX as u32, "Can't signal more than 0x7fff_ffff clients with futex");

        // We must reset the queue. Assumes there is no current user here. The old secondary
        // was apparently gone and we would be the primary.
        transaction(request);

        // Now release that state, queue is ready.
        join_futex.store(-1, Ordering::Release);
        (join_futex.as_futex() as &Futex<Shared>).wake(1);
    } else if request != u32::MAX {
        // No idea what that was but reset so that other clients can signal.
        join_futex.store(-1, Ordering::Release);
        (join_futex.as_futex() as &Futex<Shared>).wake(i32::MAX);
    }
}
