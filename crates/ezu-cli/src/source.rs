//! Abstractions over MVT tile sources: PMTiles archives (local or
//! remote) and templated `{z}/{x}/{y}` MVT sources (URL or path).

use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use ezu::core::TileId;
use pmtiles::{AsyncPmTilesReader, HttpBackend, MmapBackend, TileCoord};
use reqwest::Client;

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("pmtiles open: {0}")]
    PmTilesOpen(String),
    #[error("pmtiles read: {0}")]
    PmTilesRead(String),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("mvt file read {path}: {msg}")]
    MvtFile { path: String, msg: String },
    #[error("bad tile coord {z}/{x}/{y}: {msg}")]
    BadCoord { z: u8, x: u32, y: u32, msg: String },
    #[error("mvt pattern must contain {{z}}, {{x}}, {{y}}: {0}")]
    BadPattern(String),
    #[error("tilejson at {src}: {msg}")]
    TileJson { src: String, msg: String },
}

/// One of the supported tile-byte sources. Each `fetch` returns the
/// raw (already gzip-decompressed) MVT bytes for the requested tile,
/// or `None` if the source has no data at that coordinate.
pub enum TileSource {
    PmTilesHttp(Arc<AsyncPmTilesReader<HttpBackend>>),
    PmTilesLocal(Arc<AsyncPmTilesReader<MmapBackend>>),
    MvtHttp { pattern: String, client: Client },
    MvtFile { pattern: String },
}

impl TileSource {
    pub async fn open(spec: &SourceSpec) -> Result<Self, SourceError> {
        match spec {
            SourceSpec::PmTiles(arg) => {
                if is_url(arg) {
                    let client = Client::new();
                    let reader = AsyncPmTilesReader::new_with_url(client, arg)
                        .await
                        .map_err(|e| SourceError::PmTilesOpen(e.to_string()))?;
                    Ok(Self::PmTilesHttp(Arc::new(reader)))
                } else {
                    let reader = AsyncPmTilesReader::new_with_path(arg)
                        .await
                        .map_err(|e| SourceError::PmTilesOpen(e.to_string()))?;
                    Ok(Self::PmTilesLocal(Arc::new(reader)))
                }
            }
            SourceSpec::Mvt(arg) => {
                let pattern = if looks_like_tilejson(arg) {
                    let resolved = load_tilejson_pattern(arg).await?;
                    tracing::info!("tilejson {arg} → {resolved}");
                    resolved
                } else {
                    arg.clone()
                };
                if !pattern.contains("{z}") || !pattern.contains("{x}") || !pattern.contains("{y}")
                {
                    return Err(SourceError::BadPattern(pattern));
                }
                if is_url(&pattern) {
                    Ok(Self::MvtHttp { pattern, client: Client::new() })
                } else {
                    Ok(Self::MvtFile { pattern })
                }
            }
        }
    }

    pub async fn fetch(&self, tile: TileId) -> Result<Option<Bytes>, SourceError> {
        match self {
            Self::PmTilesHttp(r) => {
                let coord = make_coord(tile)?;
                r.get_tile_decompressed(coord)
                    .await
                    .map_err(|e| SourceError::PmTilesRead(e.to_string()))
            }
            Self::PmTilesLocal(r) => {
                let coord = make_coord(tile)?;
                r.get_tile_decompressed(coord)
                    .await
                    .map_err(|e| SourceError::PmTilesRead(e.to_string()))
            }
            Self::MvtHttp { pattern, client } => {
                let url = expand_pattern(pattern, tile);
                let resp = client.get(&url).send().await?;
                if resp.status() == reqwest::StatusCode::NOT_FOUND {
                    return Ok(None);
                }
                let resp = resp.error_for_status()?;
                Ok(Some(resp.bytes().await?))
            }
            Self::MvtFile { pattern } => {
                let path = PathBuf::from(expand_pattern(pattern, tile));
                match tokio::fs::read(&path).await {
                    Ok(buf) => Ok(Some(Bytes::from(buf))),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(SourceError::MvtFile {
                        path: path.display().to_string(),
                        msg: e.to_string(),
                    }),
                }
            }
        }
    }
}

fn expand_pattern(pattern: &str, tile: TileId) -> String {
    pattern
        .replace("{z}", &tile.z.to_string())
        .replace("{x}", &tile.x.to_string())
        .replace("{y}", &tile.y.to_string())
}

fn make_coord(tile: TileId) -> Result<TileCoord, SourceError> {
    TileCoord::new(tile.z, tile.x, tile.y).map_err(|e| SourceError::BadCoord {
        z: tile.z,
        x: tile.x,
        y: tile.y,
        msg: e.to_string(),
    })
}

/// User-supplied source choice from the CLI.
#[derive(Clone, Debug)]
pub enum SourceSpec {
    PmTiles(String),
    Mvt(String),
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Treat any `.json` argument as a TileJSON document. The TileJSON
/// spec uses no fixed suffix, but the convention is overwhelming in
/// the wild and lets the CLI dispatch without sniffing content.
fn looks_like_tilejson(arg: &str) -> bool {
    // Strip a query string before checking the extension so URLs like
    // `…/tilejson.json?key=abc` still match.
    let path_part = arg.split('?').next().unwrap_or(arg);
    path_part.to_ascii_lowercase().ends_with(".json")
}

/// Fetch a TileJSON document (URL or path) and return the first entry
/// of its `tiles` array. The spec allows multiple endpoints for load
/// balancing; we pick the first deterministically.
async fn load_tilejson_pattern(src: &str) -> Result<String, SourceError> {
    let text = if is_url(src) {
        let resp = reqwest::get(src)
            .await
            .map_err(|e| SourceError::TileJson { src: src.into(), msg: e.to_string() })?
            .error_for_status()
            .map_err(|e| SourceError::TileJson { src: src.into(), msg: e.to_string() })?;
        resp.text()
            .await
            .map_err(|e| SourceError::TileJson { src: src.into(), msg: e.to_string() })?
    } else {
        std::fs::read_to_string(src)
            .map_err(|e| SourceError::TileJson { src: src.into(), msg: e.to_string() })?
    };
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| SourceError::TileJson { src: src.into(), msg: e.to_string() })?;
    let tiles = v.get("tiles").and_then(|t| t.as_array()).ok_or_else(|| {
        SourceError::TileJson { src: src.into(), msg: "missing `tiles` array".into() }
    })?;
    let first = tiles.first().and_then(|t| t.as_str()).ok_or_else(|| {
        SourceError::TileJson { src: src.into(), msg: "`tiles[0]` is missing or not a string".into() }
    })?;
    Ok(first.to_string())
}
