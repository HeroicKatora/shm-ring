use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::spawn;

use shm_ring::{self, control::Cmd, ShmRingId};
use tempfile::tempdir;

#[test]
fn echo_once() {
    // Three processes: a controller, an echo server, an echo client.
    let shutdown = Arc::new(AtomicBool::new(true));
    let dir = tempdir().unwrap();

    let server_path = dir.path().to_owned();
    let control_path = dir.path().to_owned();

    let shutdown_server = Arc::clone(&shutdown);
    let shutdown_control = Arc::clone(&shutdown);

    let control = spawn(move || controller(control_path, shutdown_control));

    // Yeah, should do a futex wait here..
    while shutdown.load(Ordering::Relaxed) {}

    let server = spawn(move || server(server_path, shutdown_server));
    let client = spawn(move || client(dir.path().to_owned()));

    client.join().expect("Success");
    shutdown.store(true, Ordering::Release);

    server.join().expect("Server success");
    control.join().expect("Control success");
}

fn client(base: PathBuf) {
    let client = shm_ring::OpenOptions::new()
        .open(&base.join("server"))
        .unwrap();

    let mut client = client
        .join_with(shm_ring::JoinMethod::Futex)
        .connect()
        .unwrap();


    let queue = loop {
        eprintln!("[+] Sending client join");
        client.request(shm_ring::control::ServerJoin {
            tag: Default::default(),
            public_id: 0xD,
        }).unwrap();

        let (kind, queue) = loop {
            match {
                client.response(|response| {
                    if let shm_ring::control::Tag(0) = response.tag() {
                        Some((response.response(), response.value0()))
                    } else {
                        None
                    }
                })
            } {
                Ok(None) | Err(_) => {},
                Ok(Some((kind, id))) => break (kind, ShmRingId(id)),
            }
        };

        eprintln!("[+] Received response");
        if kind == Cmd::BAD {
            println!("[-] Bad request?");
            panic!("Bad request");
        }

        if kind == Cmd::UNIMPLEMENTED {
            println!("[-] Unimplemented?");
            panic!("Unimplemented");
        }

        if kind == Cmd::OUT_OF_QUEUES {
            println!("[-] Server not yet available.");
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }

        break queue;
    };

    let msg: [u8; 1] = [queue.0 as u8; 1];
    let mut queue = client
        .trust_me_with_all_queues()
        .as_client(queue);

    eprintln!("[+] Sent ping");
    queue.send(&msg).expect("No more space?");

    let ref mut buffer = [0u8; 1];
    loop {
        if let Ok(_) = queue.recv(buffer) {
            assert_eq!(*buffer, msg);
            break;
        }
    }

    eprintln!("[+] Got response");
    let queue = queue.queue_id();

    // Leave the server queue, gives it up to another client.
    while let Err(_) = client.request(shm_ring::control::LeaveRing {
        tag: Default::default(),
        queue: queue.0,
    }) { };
}

fn server(base: PathBuf, shutdown: Arc<AtomicBool>) {
    let client = shm_ring::OpenOptions::new()
        .open(&base.join("server"))
        .unwrap();

    let mut server = client
        .join_with(shm_ring::JoinMethod::Futex)
        .connect()
        .unwrap();

    println!("[+] Opening server ring {:?}.", server.client_id());
    server.request(shm_ring::control::RequestNewRing {
        tag: Default::default(),
        public_id: 0xD,
    }).unwrap();

    let (kind, listen) = loop {
        std::thread::sleep(std::time::Duration::from_millis(10));
        match {
            server.response(|response| {
                if let shm_ring::control::Tag(0) = response.tag() {
                    Some((response.response(), response.value0()))
                } else {
                    None
                }
            })
        } {
            Ok(None) => eprintln!("[-] Still no response"),
            Err(err) => eprintln!("[-] Err: {:?}", err),
            Ok(Some(id)) => break id,
        }

        println!("[+] Rechecking ring request.");
    };

    println!("[+] Got server ring on {:?}.", listen);
    if kind == Cmd::BAD {
        println!("[-] Bad request?");
        panic!("Bad request");
    }

    if kind == Cmd::UNIMPLEMENTED {
        println!("[-] Unimplemented?");
        panic!("Unimplemented command");
    }

    if kind == Cmd::OUT_OF_QUEUES {
        println!("[-] No more queues.");
        panic!("No more queues");
    }

    assert_eq!(kind, Cmd::REQUEST_NEW_RING);
    let mut queue = server
        .trust_me_with_all_queues()
        .as_server(ShmRingId(listen));

    let ref mut buffer = [0u8; 1];
    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // TODO: echo back all bytes received on the control ring
        if let Ok(_) = queue.recv(buffer) {
            println!("[+] Message received {:?}", buffer);
            while let Err(_) = queue.send(buffer) {}
            println!("[+] Answer sent");
        }
    }
}

fn controller(base: PathBuf, shutdown: Arc<AtomicBool>) {
    let mut controller = shm_ring::OpenOptions::new()
        .create(&base.join("server"))
        .unwrap();

    shutdown.store(false, Ordering::Relaxed);

    println!("Allowed methods: {}", controller.raw_join_methods());
    println!("Members (now, max): {:?}", controller.members());

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        controller.enter();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
