/// An controller hosting an shm_ring.
use std::path::Path;
use shm_ring;

fn main() {
    let controller = shm_ring::OpenOptions::new()
        .create(Path::new("server"))
        .unwrap();

    loop {
        controller.enter();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
