/// An simple, single ping to the server.
use std::path::Path;
use shm_ring;

fn main() {
    let client = shm_ring::OpenOptions::new()
        .open(Path::new("server"))
        .unwrap();

    println!("[.] Allowed methods: {}", client.raw_join_methods());
    println!("[.] Members (now, max): {:?}", client.members());

    let mut connection = client
        .join_with(shm_ring::JoinMethod::Futex)
        .connect()
        .unwrap();

    println!("[+] Joined @{}", connection.client_id());

    connection.request(shm_ring::control::Ping {
        payload: shm_ring::control::Tag(0x42),
    }).unwrap();

    println!("[+] Ping has been sent");

    loop {
        match {
            connection.response(|response| {
                shm_ring::control::Tag(0x42) == response.payload() && {
                    assert_eq!(response.operation(), shm_ring::control::ControlMessage::REQUEST_PING);
                    true
                }
            })
        } {
            Ok(false) | Err(_) => {},
            Ok(true) => break,
        }
    };

    println!("[+] Answered");
}
