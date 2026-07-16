use std::{path::PathBuf, sync::OnceLock};

use bytes::Bytes;
use freya::query::*;
use reqwest::Client;

const TILE_URL_TEMPLATE: &str = "https://vector.openstreetmap.org/shortbread_v1";

pub type TileKey = (u32, u32, u32);

#[derive(Clone, PartialEq, Hash, Eq)]
pub struct FetchTile;

impl QueryCapability for FetchTile {
    type Ok = Bytes;
    type Err = FetchError;
    type Keys = TileKey;

    async fn run(&self, keys: &Self::Keys) -> Result<Self::Ok, Self::Err> {
        let &(z, x, y) = keys;

        if let Some(path) = tile_cache_path(z, x, y) {
            if let Ok(data) = tokio::fs::read(&path).await {
                return Ok(Bytes::from(data));
            }
        }

        let data = fetch_from_network(z, x, y).await?;

        if let Some(path) = tile_cache_path(z, x, y) {
            if let Some(parent) = path.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            let _ = tokio::fs::write(&path, &data).await;
        }

        Ok(data)
    }
}

fn tile_cache_path(z: u32, x: u32, y: u32) -> Option<PathBuf> {
    dirs::cache_dir().map(|dir| {
        dir.join("rusty_map")
            .join("tiles")
            .join(z.to_string())
            .join(x.to_string())
            .join(format!("{y}.mvt"))
    })
}

fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(Client::new)
}

async fn fetch_from_network(z: u32, x: u32, y: u32) -> Result<Bytes, FetchError> {
    let url = format!("{TILE_URL_TEMPLATE}/{z}/{x}/{y}.mvt");

    let response = client()
        .get(&url)
        .send()
        .await
        .map_err(FetchError::Network)?;

    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }

    let bytes = response.bytes().await.map_err(FetchError::Network)?;

    if bytes.is_empty() {
        return Err(FetchError::NoData);
    }

    Ok(bytes)
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum FetchError {
    Network(reqwest::Error),
    NoData,
    Http(u16),
    InvalidData,
}
