/// An echo server attaching itself to a controller.
use std::path::Path;
use shm_ring::{self, control::Cmd};

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

    // TODO: setup accepting echo rings..
    server.request(shm_ring::control::RequestNewRing {
        payload: Default::default() 
    }).unwrap();

    println!("[+] Request has been sent");

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

    println!("[+] Granted {}", listen);

    loop {
        // TODO: echo back all bytes received on the control ring
    }
}
