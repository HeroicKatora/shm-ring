/// An echo server attaching itself to a controller.
use std::path::Path;
use shm_ring::{self, control::Cmd, ShmRingId};

fn main() {
    let client = shm_ring::OpenOptions::new()
        .open(Path::new("server"))
        .unwrap();

    println!("[.] Allowed methods: {}", client.raw_join_methods());
    println!("[.] Members (now, max): {:?}", client.members());

    let mut server = client
        .join_with(shm_ring::JoinMethod::Futex)
        .connect()
        .unwrap();
    println!("[+] Joined");

    server.request(shm_ring::control::RequestNewRing {
        tag: Default::default(),
        public_id: 0xD,
    }).unwrap();
    println!("[+] Server request has been sent");

    let (kind, listen) = loop {
        match {
            server.response(|response| {
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

    if kind == Cmd::BAD {
        println!("[-] Bad request?");
        std::process::exit(1);
    }

    if kind == Cmd::UNIMPLEMENTED {
        println!("[-] Unimplemented?");
        std::process::exit(1);
    }

    if kind == Cmd::OUT_OF_QUEUES {
        println!("[-] No more queues.");
        std::process::exit(1);
    }
    
    assert_eq!(kind, Cmd::REQUEST_NEW_RING);
    println!("[+] Granted {}", listen);
    let mut queue = server
        .trust_me_with_all_queues()
        .as_server(ShmRingId(listen));

    let ref mut buffer = [0u8; 1];
    loop {
        // TODO: echo back all bytes received on the control ring
        if let Ok(_) = queue.recv(buffer) {
            println!("[+] Message received {:?}", buffer);
            while let Err(_) = queue.send(buffer) {}
            println!("[+] Answer sent");
        }
    }
}
