use serde::Deserialize;

#[derive(Deserialize)]
struct Options {
    /// A version number for the configuration.
    ///
    /// Always 1.
    version: u64,
    /// The size of the backing memory to create.
    size: u64,
    /// All rings and their data to generate.
    rings: Vec<RingConfig>,
}

#[derive(Deserialize)]
struct RingConfig {
    /// A version number, for differentiating different ring layouts later.
    ///
    /// Always 1.
    version: u64,
    /// The size (in bytes) to use for the entries in the ring.
    ring_size: u64,
    /// The size of an entry in the ring structures.
    slot_entry_size: u64,
    /// The size (in bytes) of auxiliary memory to reserve for the ring's communication partners
    /// which may be used fully between them.
    data_size: u64,
}

quick_error! {
    #[derive(Debug)]
    pub enum ValidationError {
        InvalidRingSize
    }
}

fn main() {
}
