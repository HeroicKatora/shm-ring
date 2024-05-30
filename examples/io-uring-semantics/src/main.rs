mod ring;

use core::mem::MaybeUninit;
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::{rc::Rc, sync::Arc};

use io_uring::opcode;

const COOKIE_SYNC: &str = "120fadb1-ee86-4dde-8cae-20745df78e7c";
const COOKIE_EXIT: &str = "69c5b790-e304-4476-906e-716509513d02";

fn main() {
    let is_sync = std::env::args_os()
        .filter_map(|arg| arg.to_str().map(String::from))
        .any(|st| st == COOKIE_SYNC);
    let is_exit = std::env::args_os()
        .filter_map(|arg| arg.to_str().map(String::from))
        .any(|st| st == COOKIE_EXIT);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    if is_sync || is_exit {
        rt.block_on(main_pipe(is_exit)).unwrap();
    } else {
        rt.block_on(main_coordinator()).unwrap();
    }
}

async fn main_coordinator() -> Result<(), std::io::Error> {
    let this_exe = std::env::args_os().nth(0).unwrap();

    let file = tempfile::NamedTempFile::new()?;
    file.as_file().set_len(0x8)?;
    let map = memmap2::MmapRaw::map_raw(&file)?;

    let ring = ring::TestCloseRing::new()?;

    struct TestCase {
        cookie: &'static str,
        name: &'static str,
        expected_failure: Option<&'static str>,
    }

    const TESTS: &[TestCase] = &[
        TestCase {
            cookie: COOKIE_SYNC,
            name: "awaiting child ring",
            expected_failure: None,
        },
        TestCase {
            cookie: COOKIE_EXIT,
            name: "forcibly exiting child",
            expected_failure: Some("since the child didn't complete the ring ops"),
        }
    ];

    for test in TESTS {
        eprintln!("Test {}", test.name);

        let mut child = tokio::process::Command::new(&this_exe)
            .arg(file.path())
            .arg(test.cookie)
            .spawn()?;

        let submit = ring.non_empty_submission_and_then_sync(1).await?;

        let wake = opcode::FutexWait::new(
            map.as_ptr() as *const u32,
            0,
            u32::MAX.into(),
            0x02,
        )
        .build();

        unsafe { submit.redeem_push(&[wake], []) };

        tokio::select! {
            _timeout = async {
                let grace = core::time::Duration::from_millis(1000);
                tokio::time::sleep(grace).await;
            } => {
                if let Some(cause) = test.expected_failure {
                    eprintln!("  [!] Test failed, as expected, {}", cause);
                } else {
                    eprintln!("  [!] Test failed");
                }

                continue;
            },
            err = async {
                tokio::try_join! {
                    async {
                        ring.collect().await;
                        eprintln!("  [.] Futex awaited"); 
                        Ok(())
                    },
                    async {
                        child.wait().await.and_then(|out| {
                            if out.success() {
                               eprintln!("  [.] Child reaped"); 
                                Ok(())
                            } else {
                                Err(std::io::ErrorKind::Other.into())
                            }
                        })
                    },
                }
            } => err?,
        };
    }

    Ok(())
}

async fn main_pipe(exit: bool) -> Result<(), std::io::Error> {
    let file = std::env::args_os().nth(1).unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(file)?;

    let map = memmap2::MmapRaw::map_raw(&file)?;
    assert!(map.len() >= 4);
    let map = Arc::new(map);

    let (pread, _pwrite) = uapi::pipe2(0)?;

    // 1. Create a pipe.
    // 2. Open shared memory page.
    // 3. Setup linked system calls:
    //     read from pipe,
    //     futex-wake in the shared memory file
    // 4. Close the pipe
    //
    // The expected effect is a read(0) from the pipe and an associated wake. Now try to do the
    // whole thing but close the pipe implicitly by exiting the program and registering the others
    // with the ring.
    let ring = ring::TestCloseRing::new()?;
    ring.collect().await;

    let submit = ring.non_empty_submission_and_then_sync(2).await?;

    let into = Box::leak(Box::new(MaybeUninit::<[u8; 1]>::uninit()));
    let fd = io_uring::types::Fd(pread.as_fd().as_raw_fd());

    let read = opcode::Read::new(fd, into.as_ptr() as *mut _, 1)
        .build()
        .flags(io_uring::squeue::Flags::IO_HARDLINK);

    let wake = opcode::FutexWake::new(
        map.as_ptr() as *const u32,
        u32::MAX.into(),
        u32::MAX.into(),
        0x02,
    )
    .build();

    unsafe { submit.redeem_push(&[read, wake], [Rc::new(map.clone())]) };

    if exit {
        std::process::exit(0);
    }

    uapi::close(_pwrite)?;
    ring.collect().await;

    Ok(())
}
