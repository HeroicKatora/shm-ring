/// An controller hosting an shm_ring.
use shm_ring;

fn main() {
    let mut controller = shm_ring::OpenOptions::new()
        .create("shm-ring-example-server")
        .unwrap();

    println!("Allowed methods: {}", controller.raw_join_methods());
    println!("Members (now, max): {:?}", controller.members());

    loop {
        controller.enter();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
