mod stream;

use shm_pbx::client::{RingRequest, WaitResult};
use shm_pbx::data::{ClientIdentifier, ClientSide, RingIndex};
use shm_pbx::frame::Shared;
use shm_pbx::server::{RingConfig, RingVersion, ServerConfig};

use memmap2::MmapRaw;
use serde::Deserialize;
use std::{fs, path::PathBuf, time::Duration};

#[derive(Deserialize)]
struct Options {
    index: usize,
    #[serde(deserialize_with = "de::deserialize_client")]
    side: ClientSide,
}

fn main() -> std::io::Result<()> {
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

    let server = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(server)?;

    let map = MmapRaw::map_raw(&server).unwrap();
    // Fulfills all the pre-conditions of alignment to map.
    let shared = Shared::new(map).unwrap();
    let client = shared.into_client().expect("Have initialized client");

    let tid = ClientIdentifier::new();
    client.join(&RingRequest {
        index: RingIndex(options.index),
        side: options.side,
        tid,
    }).unwrap();

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
