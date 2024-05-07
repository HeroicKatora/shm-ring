use core::{num::NonZeroU64, time::Duration};
use std::{fs, path::PathBuf};

use shm_pbx::client::RingRequest;
use shm_pbx::data::{ClientIdentifier, ClientSide, RingIndex};
use shm_pbx::frame::Shared;
use shm_pbx::io_uring::ShmIoUring;
use shm_pbx::server::{RingConfig, RingVersion, ServerConfig, ServerError, ServerTask};

use memmap2::MmapRaw;
use quick_error::quick_error;
use serde::Deserialize;
use tempfile::NamedTempFile;

#[derive(Deserialize)]
struct Options {
    /// A version number for the configuration.
    ///
    /// Always 1.
    version: u64,
    /// The size of the backing memory to create.
    size: u64,
    /// All rings and their data to generate.
    rings: Vec<RingOptions>,
    /// Duration used when sleeping on a client trigger, in nano seconds.
    ///
    /// Note that this is not a latency but rather a maximum time before waking, not a minimum
    /// time.
    ///
    /// Providing no value uses some arbitrary default below a second. (FIXME: motivation for this
    /// default).
    sleep: Option<NonZeroU64>,
}

#[derive(Deserialize)]
struct RingOptions {
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
    /// The ID to advertise on the left of this ring when it falls to the server.
    lhs_id: Option<i64>,
    /// The ID to advertise on the right of this ring when it falls to the server.
    rhs_id: Option<i64>,
}

quick_error! {
    #[derive(Debug)]
    pub enum ValidationError {
        InvalidRingSize
    }
}

quick_error! {
    #[derive(Debug)]
    pub enum ServerServeError {
        BadInternalOptions (err: ServerError) {
            from()
        }
        IoUring (err: std::io::Error) {
            from(err: ServeIoUring) -> (err.0)
        }
    }
}

/// An IO-error that originates from serving the IO-uring.
///
/// Distinguishes those at the type-level to make `ServerServeError::from` work.
struct ServeIoUring(std::io::Error);

quick_error! {
    #[derive(Debug)]
    pub enum OptionsError {
        UnsupportedVersion {}
        FailedToUseMap {}
    }
}

quick_error! {
    #[derive(Debug)]
    pub enum Error {
        BadOptions (err: OptionsError) {
            from()
        }
        Serve(err: ServerServeError) {
            from()
        }
        Config (err: serde_json::Error) {
            from()
        }
        Io (err: std::io::Error) {
            from()
        }
    }
}

fn main() -> Result<(), Error> {
    let matches = clap::command!()
        .arg(
            clap::arg!(
                -c --config <FILE> "A json configuration file"
            )
            .required(true)
            .value_parser(clap::value_parser!(PathBuf)),
        )
        .get_matches();

    let Some(config) = matches.get_one::<PathBuf>("config") else {
        panic!("Parser validation failed");
    };

    let file = fs::File::open(config)?;
    let options: Options = serde_json::de::from_reader(file)?;

    let file = NamedTempFile::new()?;
    file.as_file().set_len(options.size)?;
    let map = MmapRaw::map_raw(&file)?;

    if options.version != 1 {
        return Err(OptionsError::UnsupportedVersion)?;
    }

    if options.rings.iter().any(|opt| opt.version != 1) {
        return Err(OptionsError::UnsupportedVersion)?;
    }

    // Fulfills all the pre-conditions of alignment to map.
    let shared = Shared::new(map).ok_or(OptionsError::FailedToUseMap)?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()?;

    rt.block_on(serve_map_in(shared, options))?;

    Ok(())
}

async fn serve_map_in(shared: Shared, options: Options) -> Result<(), ServerServeError> {
    let uring = ShmIoUring::new(&shared).map_err(ServeIoUring)?;

    let rings: Vec<_> = options
        .rings
        .iter()
        .map(|opt| RingConfig {
            version: RingVersion::default(),
            ring_size: opt.ring_size,
            data_size: opt.data_size,
            slot_entry_size: opt.slot_entry_size,
        })
        .collect();

    let duration = options.sleep.map_or(10_000_000, NonZeroU64::get);
    let duration = Duration::from_nanos(duration);
    let server = unsafe { shared.into_server(ServerConfig { vec: &rings }) }?;
    let mut task = ServerTask::default();

    loop {
        uring
            .wait_server(&server, duration)
            .await
            .map_err(ServeIoUring)?;

        server.collect_fds(&mut task);

        std::hint::spin_loop();
        std::thread::yield_now();
    }
}
