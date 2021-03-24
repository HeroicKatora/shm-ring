/// An echo server attaching itself to a controller.
use std::path::Path;
use shm_ring::{self, control::Cmd, ShmRingId};

fn main() {
    let client = shm_ring::OpenOptions::new()
        .open(Path::new("server"))
        .unwrap();

    println!("[.] Allowed methods: {}", client.raw_join_methods());
    println!("[.] Members (now, max): {:?}", client.members());

    let mut client = client
        .join_with(shm_ring::JoinMethod::Futex)
        .connect()
        .unwrap();
    println!("[+] Joined");

    // First check that the wrong ID does not give us a server but returns.
    {
        client.request(shm_ring::control::ServerJoin {
            tag: Default::default(),
            public_id: 0xBAD,
        }).unwrap();

        loop {
            match {
                client.response(|response| {
                    if let shm_ring::control::Tag(0) = response.tag() {
                        assert_eq!(response.response(), Cmd::OUT_OF_QUEUES);
                        true
                    } else {
                        false
                    }
                })
            } {
                Ok(false) | Err(_) => {},
                Ok(true) => break,
            }
        };
    }

    client.request(shm_ring::control::ServerJoin {
        tag: Default::default(),
        public_id: 0xD,
    }).unwrap();
    println!("[+] Server request has been sent");

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
            Ok(Some(id)) => break id,
        }
    };

    let queue = ShmRingId(queue);

    if kind == Cmd::BAD {
        println!("[-] Bad request?");
        std::process::exit(1);
    }

    if kind == Cmd::UNIMPLEMENTED {
        println!("[-] Unimplemented?");
        std::process::exit(1);
    }

    if kind == Cmd::OUT_OF_QUEUES {
        println!("[-] No server available.");
        std::process::exit(1);
    }

    println!("[+] Server found {}", queue.0);
    let msg: [u8; 1] = [queue.0 as u8; 1];
    let mut queue = client.trust_me_with_all_queues().as_client(queue);

    queue.send(&msg).expect("No more space?");
    println!("[+] Message sent");

    let ref mut buffer = [0u8; 1];
    loop {
        if let Ok(_) = queue.recv(buffer) {
            println!("[+] Answer received");
            assert_eq!(*buffer, msg);
            break;
        }
    }

    let queue = queue.queue_id();

    // Leave the server queue, gives it up to another client.
    while let Err(_) = client.request(shm_ring::control::LeaveRing {
        tag: Default::default(),
        queue: queue.0,
    }) { };
}

