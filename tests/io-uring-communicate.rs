#![cfg(feature = "io-uring")]
use user_ring::client::{Ring, RingRequest, WaitResult};
use user_ring::data::{ClientIdentifier, ClientSide, RingIndex};
use user_ring::frame::Shared;
use user_ring::io_uring::ShmIoUring;
use user_ring::server::{RingConfig, RingVersion, Server, ServerConfig};

use memmap2::MmapRaw;
use std::{collections::VecDeque, hash::Hash as _, time::Duration};
use tempfile::NamedTempFile;

#[tokio::test(flavor = "multi_thread")]
async fn sync_rings() {
    let (shared, _server) = _setup_server();

    let shared_client = shared.clone().into_client();
    let client = shared_client.expect("Have initialized client");

    let tid = ClientIdentifier::new();
    let join_lhs = client.join(&RingRequest {
        side: ClientSide::Left,
        index: RingIndex(0),
        tid,
    });

    let join_rhs = client.join(&RingRequest {
        side: ClientSide::Right,
        index: RingIndex(0),
        tid,
    });

    let lhs = join_lhs.expect("Have initialized left side");
    let rhs = join_rhs.expect("Have initialized right side");

    let ioring = ShmIoUring::new(&shared).unwrap();

    assert!(
        ioring.is_supported().expect("Probing works").any(),
        "Your OS does not support the Futex operations required"
    );

    rhs.activate();
    lhs.activate();

    let (rhs, lhs) = tokio::join!(producer_task(rhs), consumer_task(lhs));
    assert_eq!(rhs.2, lhs.2);

    let mut pairs = rhs.1.iter().zip(lhs.1.iter());
    if let Some(pos) = pairs.position(|(a, b)| *a != *b) {
        // Definitely fails if we are in the if, only for printing the fault
        assert_eq!(rhs.1[pos..], lhs.1[pos..]);
    }

    assert_eq!(rhs.0, lhs.0);
}

const PAGE_SIZE: usize = 4096;

async fn producer_task(mut ring: Ring) -> (u64, Vec<u8>, Vec<u64>) {
    let attributes = ring.info();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let frame_count = attributes.size_data / PAGE_SIZE as u64;

    let cmd_image = std::env::args().next().unwrap();
    let data = tokio::fs::read(cmd_image).await.unwrap();
    let mut data = data.chunks(4096);

    // Initially everything is empty.
    let mut empty_frames: Vec<_> = (0..frame_count).collect();
    let mut full_frames = VecDeque::new();
    let mut buffer = vec![0; PAGE_SIZE];
    let mut all_data = vec![];
    let mut frame_order = vec![];

    let mut fill_frames = |empty: &mut Vec<u64>, full: &mut VecDeque<u64>, ring: &Ring| {
        while let Some(fidx) = empty.pop() {
            buffer.clear();

            let Some(chars) = data.next() else {
                empty.push(fidx);
                return false;
            };

            buffer.extend_from_slice(chars);
            buffer.resize(PAGE_SIZE, 0u8);
            u8::hash_slice(&buffer, &mut hasher);
            frame_order.push(fidx);
            all_data.extend_from_slice(&buffer);

            let at = fidx * PAGE_SIZE as u64;
            unsafe { ring.copy_to(&buffer, at) };
            full.push_back(fidx);
        }

        true
    };

    loop {
        // FIXME: idiomatically, why the need for an unwrap here. Should we not check this property
        // statically when you join the ring or allow a transform from an unsized ring to a
        // concretely sized ring in the type system.
        let mut reap = ring.consumer::<8>().unwrap();
        empty_frames.extend(reap.iter().map(u64::from_le_bytes));
        reap.sync();

        if !fill_frames(&mut empty_frames, &mut full_frames, &ring) && full_frames.is_empty() {
            break;
        }

        let mut produce = ring.producer::<8>().unwrap();
        produce.push_many(core::iter::from_fn(|| {
            let fidx = full_frames.pop_front()?;
            Some(fidx.to_le_bytes())
        }));
        produce.sync();

        tokio::task::yield_now().await;
    }

    ring.deactivate();

    (core::hash::Hasher::finish(&hasher), all_data, frame_order)
}

async fn consumer_task(mut ring: Ring) -> (u64, Vec<u8>, Vec<u64>) {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();

    let mut ready_frames = VecDeque::new();
    let mut done_frames = vec![];
    let mut buffer = vec![0; PAGE_SIZE];
    let mut all_data = vec![];
    let mut frame_order = vec![];

    let mut dump_frames = |ready: &mut VecDeque<u64>, done: &mut Vec<u64>, ring: &Ring| {
        let drain = ready.drain(..);
        let drain = drain.inspect(|&fidx| {
            frame_order.push(fidx);
            let at = fidx * PAGE_SIZE as u64;
            unsafe { ring.copy_from(buffer.as_mut_slice(), at) };
            u8::hash_slice(&buffer, &mut hasher);
            all_data.extend_from_slice(&buffer);
        });

        done.extend(drain);
    };

    while ring.is_active_remote() {
        let mut recv = ring.consumer::<8>().unwrap();
        recv.removable();
        ready_frames.extend(recv.iter().map(u64::from_le_bytes));
        recv.sync();

        dump_frames(&mut ready_frames, &mut done_frames, &ring);

        let mut returnf = ring.producer::<8>().unwrap();
        returnf.push_many(core::iter::from_fn(|| {
            let fidx = done_frames.pop()?;
            Some(fidx.to_le_bytes())
        }));
        returnf.sync();

        tokio::task::yield_now().await;
    }

    let mut recv = ring.consumer::<8>().unwrap();
    ready_frames.extend(recv.iter().map(u64::from_le_bytes));
    recv.sync();

    dump_frames(&mut ready_frames, &mut done_frames, &ring);

    (core::hash::Hasher::finish(&hasher), all_data, frame_order)
}

fn _setup_server() -> (Shared, Server) {
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
        ring_size: 0x1_000,
        // 0x100 frames of page size each.
        data_size: 0x400_000,
        slot_entry_size: 0x8,
    }];

    let shared_server = shared.clone();
    let server = unsafe { shared_server.into_server(ServerConfig { vec: &rings }) };
    let server = server.expect("Have initialized server");

    (shared, server)
}
