use shm_pbx::client::RingRequest;
use shm_pbx::data::{ClientIdentifier, ClientSide, RingIndex};
use shm_pbx::frame::Shared;
use shm_pbx::server::{RingConfig, RingVersion, ServerConfig};

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

    let rings = [RingConfig {
        version: RingVersion::default(),
        ring_size: 0x10,
        data_size: 0x1234,
        slot_entry_size: 0x8,
    }];

    let shared_client = shared.clone();
    assert!(shared_client.into_client().is_err());

    let shared_server = shared.clone();
    let server = unsafe { shared_server.into_server(ServerConfig { vec: &rings }) };
    let server = server.expect("Have initialized server");

    let shared_client = shared.clone().into_client();
    let client = shared_client.expect("Have initialized client");

    let tid = ClientIdentifier::new();
    let join_lhs = client.join(&RingRequest {
        side: ClientSide::Left,
        index: RingIndex(0),
        tid,
    });

    assert!(join_lhs.is_ok());

    {
        // We can't join this another time..
        let join = client.join(&RingRequest {
            side: ClientSide::Left,
            index: RingIndex(0),
            tid,
        });

        assert!(join.is_err());
    }

    {
        // We can't join this non-existing ring.
        let join = client.join(&RingRequest {
            side: ClientSide::Left,
            index: RingIndex(1),
            tid,
        });

        assert!(join.is_err());
    }

    let tid = ClientIdentifier::new();
    let join_rhs = client.join(&RingRequest {
        side: ClientSide::Right,
        index: RingIndex(0),
        tid,
    });

    assert!(join_rhs.is_ok());
    drop(join_rhs);

    // We can join it again.
    let tid = ClientIdentifier::new();
    let join_rhs = client.join(&RingRequest {
        side: ClientSide::Right,
        index: RingIndex(0),
        tid,
    });

    assert!(join_rhs.is_ok());
    let _ = (server, client);
}
