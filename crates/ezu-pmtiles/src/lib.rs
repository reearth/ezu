//! PMTiles reader for ezu.
//!
//! Thin async wrappers around the `pmtiles` crate that fetch the decompressed
//! MVT bytes for an [`ezu_core::TileId`] from either a local file (mmap) or
//! an HTTP URL (range requests).

use std::path::Path;

use bytes::Bytes;
use ezu_core::TileId;
use pmtiles::{AsyncPmTilesReader, HttpBackend, MmapBackend, TileCoord};

#[derive(Debug, thiserror::Error)]
pub enum PmTilesError {
    #[error("pmtiles: {0}")]
    Pmtiles(#[from] pmtiles::PmtError),
    #[error("http client: {0}")]
    Http(#[from] reqwest::Error),
}

enum Backend {
    Mmap(AsyncPmTilesReader<MmapBackend>),
    Http(AsyncPmTilesReader<HttpBackend>),
}

/// Read MVT tiles from a PMTiles archive (local mmap or remote HTTP).
pub struct PmTilesArchive {
    backend: Backend,
}

impl PmTilesArchive {
    /// Open a local `.pmtiles` file via memory-mapped I/O.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, PmTilesError> {
        let inner = AsyncPmTilesReader::new_with_path(path.as_ref()).await?;
        Ok(Self {
            backend: Backend::Mmap(inner),
        })
    }

    /// Open a remote `.pmtiles` archive via HTTP range requests.
    pub async fn open_url(url: &str) -> Result<Self, PmTilesError> {
        let client = reqwest::Client::new();
        let inner = AsyncPmTilesReader::new_with_url(client, url).await?;
        Ok(Self {
            backend: Backend::Http(inner),
        })
    }

    /// Archive header (min/max zoom, tile type, compression, …).
    pub fn header(&self) -> &pmtiles::Header {
        match &self.backend {
            Backend::Mmap(r) => r.get_header(),
            Backend::Http(r) => r.get_header(),
        }
    }

    /// Fetch a single tile as decompressed bytes. Returns `Ok(None)` if the
    /// archive does not contain that tile.
    pub async fn get_tile(&self, tile: TileId) -> Result<Option<Bytes>, PmTilesError> {
        let coord = TileCoord::new(tile.z, tile.x, tile.y)?;
        Ok(match &self.backend {
            Backend::Mmap(r) => r.get_tile_decompressed(coord).await?,
            Backend::Http(r) => r.get_tile_decompressed(coord).await?,
        })
    }
}
