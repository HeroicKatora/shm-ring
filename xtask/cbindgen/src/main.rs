//! This is an xtask specific to this repository. It must be ran with the cargo alias provided.
fn main() {
    let crate_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../");
    let crate_dir = std::path::Path::new(&crate_dir);

    let config = cbindgen::Config::from_file(crate_dir.join("cbindgen.toml"))
        .expect("Unable to read binding configuration");

    cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_config(config)
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file("include/user_ring.h");
}
