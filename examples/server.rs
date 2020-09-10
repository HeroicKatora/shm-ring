/// An echo server attaching itself to a controller.
use std::path::Path;
use shm_ring;

fn main() {
    let mut client = shm_ring::OpenOptions::new()
        .open(Path::new("server"))
        .unwrap();

    let mut server = client.connect().unwrap();
    // TODO: setup accepting echo rings..
    server.request(shm_ring::control::RequestNewRing {
        payload: Default::default() 
    }).unwrap();

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

    loop {
        // TODO: echo back all bytes received on the control ring
    }
}
