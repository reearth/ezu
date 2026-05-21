//! Thin async wrapper around the `pmtiles` crate. Mirrors the small
//! interface ezu-server needs (`open_url`, `header`, `get_tile`) without
//! depending on a dedicated workspace crate.

use bytes::Bytes;
use ezu::core::TileId;
use pmtiles::{AsyncPmTilesReader, HttpBackend, TileCoord};

#[derive(Debug, thiserror::Error)]
pub enum PmTilesError {
    #[error("pmtiles: {0}")]
    Pmtiles(#[from] pmtiles::PmtError),
    #[error("http client: {0}")]
    Http(#[from] reqwest::Error),
}

pub struct PmTilesArchive {
    inner: AsyncPmTilesReader<HttpBackend>,
}

impl PmTilesArchive {
    pub async fn open_url(url: &str) -> Result<Self, PmTilesError> {
        let client = reqwest::Client::new();
        let inner = AsyncPmTilesReader::new_with_url(client, url).await?;
        Ok(Self { inner })
    }

    pub async fn get_tile(&self, tile: TileId) -> Result<Option<Bytes>, PmTilesError> {
        let coord = TileCoord::new(tile.z, tile.x, tile.y)?;
        Ok(self.inner.get_tile_decompressed(coord).await?)
    }
}
