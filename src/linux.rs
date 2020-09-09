use super::{ConnectError, ShmClient};

use core::sync::atomic::Ordering;
use linux_futex::{AsFutex, Futex, Shared, WaitError};

pub fn own_pid() -> u64 {
    unsafe { libc::getpid() as u64 }
}

pub fn futex(client: &ShmClient, this: u32) -> Result<u32, ConnectError> {
    let open = client.open();

    let futex: &Futex<Shared> = open.futex.as_futex();
    while let Err(err) = futex.value.compare_exchange(0, this as i32, Ordering::Relaxed, Ordering::Relaxed) {
        futex.wake(1);
        // Shortly wait for this to clear. Never mind if it did already.
        let _ = futex.wait(err);
    }

    loop {
        match futex.wait(this as i32) {
            // If someone else updated before we woke, we might have a wrong value but succeeded.
            Ok(()) | Err(WaitError::WrongValue) => break,
            Err(WaitError::Interrupted) => {},
        }
    }

    // Queues are ID mapped for clients..
    Ok(this)
}
