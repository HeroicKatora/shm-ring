use super::{ConnectError, ShmClient, ShmController};

use core::sync::atomic::Ordering;
use linux_futex::{AsFutex, Futex, Shared, WaitError};

pub fn own_pid() -> u64 {
    unsafe { libc::getpid() as u64 }
}

pub fn futex(client: &ShmClient, this: u32) -> Result<u32, ConnectError> {
    let open = client.open();
    let open = &open.futex;

    while let Err(err) = open.compare_exchange(-1, this as i32, Ordering::AcqRel, Ordering::Relaxed) {
        let futex: &Futex<Shared> = open.as_futex();
        futex.wake(1);
        // Shortly wait for this to clear. Never mind if it did already.
        let _ = futex.wait(err);
    }

    eprintln!("[+] Waiting @{}", this);

    loop {
        match (open.as_futex() as &Futex<Shared>).wait(this as i32) {
            // If someone else updated before we woke, we might have a wrong value but succeeded.
            Ok(()) | Err(WaitError::WrongValue) => break,
            Err(WaitError::Interrupted) => {},
        }
    }

    // Queues are ID mapped for clients..
    Ok(this)
}

pub fn join_futex(server: &ShmController, transaction: impl FnOnce(u32)) {
    let join_futex = &server.open().futex;
    let request = join_futex.load(Ordering::Acquire) as u32;
    if request < server.monitor().max_members.load(Ordering::Relaxed) {
        // We must reset the queue. Assumes there is no current user here. The old secondary
        // was apparently gone and we would be the primary.
        eprintln!("[+] Joined @{}", request);
        transaction(request);

        // Now release that state, queue is ready.
        join_futex.store(-1, Ordering::Release);
        (join_futex.as_futex() as &Futex<Shared>).wake(1);
    } else if request != u32::MAX {
        eprintln!("[-] Fail {}", request);
        // No idea what that was but reset so that other clients can signal.
        join_futex.store(-1, Ordering::Release);
        (join_futex.as_futex() as &Futex<Shared>).wake(1);
    }
}
