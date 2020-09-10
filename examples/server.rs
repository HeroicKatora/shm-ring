/// An echo server attaching itself to a controller.
use std::path::Path;
use shm_ring;

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

    let listen = loop {
        match {
            server.response(|response| {
                if let shm_ring::control::Tag(0) = response.payload() {
                    Some(response.operation())
                } else {
                    None
                }
            })
        } {
            Ok(None) | Err(_) => {},
            Ok(Some(id)) => break id,
        }
    };

    if listen == u32::MAX {
        println!("[-] Bad request?");
        std::process::exit(1);
    }

    if listen == u32::MAX - 1 {
        println!("[-] Unimplemented?");
        std::process::exit(1);
    }

    println!("[+] Granted {}", listen);

    loop {
        // TODO: echo back all bytes received on the control ring
    }
}
