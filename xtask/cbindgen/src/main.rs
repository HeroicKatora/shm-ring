//! This is an xtask specific to this repository. It must be ran with the cargo alias provided.
fn main() {
    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    cbindgen::Builder::new()
        .with_crate(crate_dir)
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file("include/user_ring.h");
}
