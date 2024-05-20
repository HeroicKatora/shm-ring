mod stream;

use shm_pbx::client::{Ring, RingRequest, WaitResult};
use shm_pbx::data::{ClientIdentifier, ClientSide, RingIndex};
use shm_pbx::frame::Shared;
use shm_pbx::io_uring::ShmIoUring;
use shm_pbx::server::{RingConfig, RingVersion, ServerConfig};

use memmap2::MmapRaw;
use quick_error::quick_error;
use serde::Deserialize;
use wasmtime::{component::Component, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx};

use std::{fs, path::PathBuf, sync};

#[derive(Deserialize)]
struct Options {
    module: PathBuf,
    stdin: Option<StreamOptions>,
    stdout: Option<StreamOptions>,
    stderr: Option<StreamOptions>,
}

#[derive(Deserialize)]
struct StreamOptions {
    index: usize,
    #[serde(deserialize_with = "de::deserialize_client")]
    side: ClientSide,
}

struct MainData {
    ctx: WasiCtx,
    table: ResourceTable,
}

struct Program {
    module: Component,
    engine: wasmtime::Engine,
    stdin: Option<Ring>,
    stdout: Option<Ring>,
    stderr: Option<Ring>,
}

quick_error! {
    #[derive(Debug)]
    pub enum ClientRunError {
        IoUring (err: std::io::Error) {
            from(err: ClientIoUring) -> (err.0)
        }
        UnsupportedOs {}
        WasmModule (err: wasmtime::Error) {
            from()
        }
    }
}

/// An IO-error that originates from serving the IO-uring.
///
/// Distinguishes those at the type-level to make `ClientRunError::from` work.
struct ClientIoUring(std::io::Error);

quick_error! {
    #[derive(Debug)]
    pub enum Error {
        Serve(err: ClientRunError) {
            from()
        }
        Config (err: serde_json::Error) {
            from()
        }
        Io (err: std::io::Error) {
            from()
        }
        WasmModule (err: wasmtime::Error) {
            from()
        }
    }
}

impl wasmtime_wasi::WasiView for MainData {
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }

    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.ctx
    }
}

fn main() -> Result<(), Error> {
    let matches = clap::command!()
        .arg(
            clap::arg!(
                <server> "The serve to join"
            )
            .required(true)
            .value_parser(clap::value_parser!(PathBuf)),
        )
        .arg(
            clap::arg!(
                -c --config <FILE> "A json configuration file"
            )
            .required(true)
            .value_parser(clap::value_parser!(PathBuf)),
        )
        .get_matches();

    let Some(server) = matches.get_one::<PathBuf>("server") else {
        panic!("Parser validation failed");
    };

    let Some(config) = matches.get_one::<PathBuf>("config") else {
        panic!("Parser validation failed");
    };

    let file = fs::File::open(config)?;
    let options: Options = serde_json::de::from_reader(file)?;
    let opt_path = config.parent().unwrap();

    let server = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(server)?;

    let map = MmapRaw::map_raw(&server).unwrap();
    // Fulfills all the pre-conditions of alignment to map.
    let shared = Shared::new(map).unwrap();
    let client = shared.into_client().expect("Have initialized client");

    let tid = ClientIdentifier::new();

    let stdin = options
        .stdin
        .map(|opt| {
            client.join(&RingRequest {
                index: RingIndex(opt.index),
                side: opt.side,
                tid,
            })
        })
        .transpose()
        .unwrap();

    let mut config = wasmtime::Config::new();
    config.async_support(true);
    let engine = wasmtime::Engine::new(&config)?;

    let module = opt_path.join(options.module);
    eprintln!("{}", module.display());
    module.canonicalize()?;

    let module = std::fs::read(module)?;
    let module = wasmtime::component::Component::new(&engine, module)?;

    let program = Program {
        module,
        engine: engine.clone(),
        stdin,
        stdout: None,
        stderr: None,
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()?;

    rt.block_on(communicate(shared, program))?;

    Ok(())
}

struct Stdin {
    inner: stream::InputRing,
}

impl wasmtime_wasi::StdinStream for Stdin {
    fn stream(&self) -> Box<dyn wasmtime_wasi::HostInputStream> {
        Box::new(self.inner.clone())
    }

    fn isatty(&self) -> bool {
        false
    }
}

async fn communicate(shared: Shared, program: Program) -> Result<(), ClientRunError> {
    let uring = ShmIoUring::new(&shared).map_err(ClientIoUring)?;
    let uring = sync::Arc::new(uring);

    let main = MainData {
        table: ResourceTable::new(),
        ctx: {
            let mut ctx = WasiCtx::builder();

            if let Some(stdin) = program.stdin {
                let stdin = Stdin {
                    inner: stream::InputRing::new(stdin, uring.clone()),
                };

                ctx.stdin(stdin);
            }

            ctx.build()
        },
    };

    let mut linker = wasmtime::component::Linker::<MainData>::new(&program.engine);

    let mut store = Store::new(&program.engine, main);
    wasmtime_wasi::add_to_linker_async(&mut linker)?;

    let instance = linker
        .instantiate_async(&mut store, &program.module)
        .await?;

    let main = instance.get_typed_func::<(), (i32,)>(&mut store, "main")?;
    main.call_async(&mut store, ()).await?;

    Ok(())
}

mod de {
    use serde::{Deserialize, Deserializer};

    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum ClientSide {
        Left,
        Right,
    }

    pub fn deserialize_client<'de, D>(de: D) -> Result<super::ClientSide, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match ClientSide::deserialize(de)? {
            ClientSide::Left => super::ClientSide::Left,
            ClientSide::Right => super::ClientSide::Right,
        })
    }
}
