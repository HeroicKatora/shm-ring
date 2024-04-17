use std::fs::File;

use user_ring::frame::Shared;
use memmap2::MmapRaw;

#[test]
fn create_server() {
    let file = File::create("target/testfile").unwrap();
    file.truncate(0x1_000_000).unwrap();

    let map = MmapRaw::map_raw(&file).unwrap();
}
