use user_ring::frame::Shared;
use user_ring::server::{ServerConfig, RingConfig, RingVersion};

use memmap2::MmapRaw;
use tempfile::NamedTempFile;

#[test]
fn create_server() {
    // Please do not use this in reality. This isn't guaranteed to be in a shared memory region and
    // you'll land in a cache—which isn't compatible with the memory model requirements to
    // communicate effectively. (Or at all, not sure).
    let file = NamedTempFile::new().unwrap();
    file.as_file().set_len(0x1_000_000).unwrap();

    let map = MmapRaw::map_raw(&file).unwrap();
    // Fulfills all the pre-conditions of alignment to map.
    let shared = Shared::new(map).unwrap();

    let rings = [
        RingConfig {
            version: RingVersion::default(),
            ring_size: 0x10,
            data_size: 0x1234,
            slot_entry_size: 0x8,
        }
    ];

    let server = unsafe {
        shared.into_server(ServerConfig {
            vec: &rings,
        })
    };

    assert!(server.is_ok());
}
