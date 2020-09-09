/// An echo server attaching itself to a controller.
use std::path::Path;
use shm_ring;

fn main() {
    let mut client = shm_ring::OpenOptions::new()
        .open(Path::new("server"))
        .unwrap();

    let server = client.connect()?;
    // TODO: setup accepting echo rings..

    loop {
        // TODO: echo back all bytes received on the control ring
    }
}
