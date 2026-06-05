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
/// or `None` if the source has no data at that coordinate. `open`
/// captures upstream attribution metadata (TileJSON `attribution`,
/// PMTiles archive metadata) for hosts to surface.
pub struct TileSource {
    kind: TileSourceKind,
    attribution: Option<String>,
}

enum TileSourceKind {
    PmTilesHttp(Arc<AsyncPmTilesReader<HttpBackend>>),
    PmTilesLocal(Arc<AsyncPmTilesReader<MmapBackend>>),
    MvtHttp { pattern: String, client: Client },
    MvtFile { pattern: String },
}

impl TileSource {
    pub async fn open(spec: &SourceSpec) -> Result<Self, SourceError> {
        match spec {
            SourceSpec::PmTiles(arg) => {
                let (kind, metadata) = if is_url(arg) {
                    let client = Client::new();
                    let reader = AsyncPmTilesReader::new_with_url(client, arg)
                        .await
                        .map_err(|e| SourceError::PmTilesOpen(e.to_string()))?;
                    let meta = reader.get_metadata().await.ok();
                    (TileSourceKind::PmTilesHttp(Arc::new(reader)), meta)
                } else {
                    let reader = AsyncPmTilesReader::new_with_path(arg)
                        .await
                        .map_err(|e| SourceError::PmTilesOpen(e.to_string()))?;
                    let meta = reader.get_metadata().await.ok();
                    (TileSourceKind::PmTilesLocal(Arc::new(reader)), meta)
                };
                let attribution = metadata.and_then(|m| {
                    serde_json::from_str::<serde_json::Value>(&m)
                        .ok()?
                        .get("attribution")?
                        .as_str()
                        .map(str::to_string)
                });
                Ok(Self { kind, attribution })
            }
            SourceSpec::Mvt(arg) => {
                let (pattern, attribution) = if looks_like_tilejson(arg) {
                    let (resolved, attribution) = load_tilejson_pattern(arg).await?;
                    tracing::info!("tilejson {arg} → {resolved}");
                    (resolved, attribution)
                } else {
                    (arg.clone(), None)
                };
                if !pattern.contains("{z}") || !pattern.contains("{x}") || !pattern.contains("{y}")
                {
                    return Err(SourceError::BadPattern(pattern));
                }
                let kind = if is_url(&pattern) {
                    TileSourceKind::MvtHttp {
                        pattern,
                        client: Client::new(),
                    }
                } else {
                    TileSourceKind::MvtFile { pattern }
                };
                Ok(Self { kind, attribution })
            }
        }
    }

    /// Upstream attribution captured at open time (TileJSON
    /// `attribution` field, PMTiles metadata), if any.
    pub fn attribution(&self) -> Option<&str> {
        self.attribution.as_deref()
    }

    /// Fetch a tile, walking up the pyramid up to `max_parent_levels`
    /// times on `None` (404 / missing). Returns the raw bytes together
    /// with the `TileId` they actually came from — the caller passes
    /// that to [`ezu_features::mvt::clip_to_descendant`] to remap the
    /// parent's coordinate frame onto the requested tile.
    ///
    /// `max_parent_levels = 0` is identical to [`Self::fetch`].
    pub async fn fetch_with_fallback(
        &self,
        tile: TileId,
        max_parent_levels: u8,
    ) -> Result<Option<(Bytes, TileId)>, SourceError> {
        let mut current = tile;
        for _ in 0..=max_parent_levels {
            if let Some(bytes) = self.fetch(current).await? {
                return Ok(Some((bytes, current)));
            }
            let Some(parent) = current.parent() else {
                break;
            };
            current = parent;
        }
        Ok(None)
    }

    pub async fn fetch(&self, tile: TileId) -> Result<Option<Bytes>, SourceError> {
        match &self.kind {
            TileSourceKind::PmTilesHttp(r) => {
                let coord = make_coord(tile)?;
                r.get_tile_decompressed(coord)
                    .await
                    .map_err(|e| SourceError::PmTilesRead(e.to_string()))
            }
            TileSourceKind::PmTilesLocal(r) => {
                let coord = make_coord(tile)?;
                r.get_tile_decompressed(coord)
                    .await
                    .map_err(|e| SourceError::PmTilesRead(e.to_string()))
            }
            TileSourceKind::MvtHttp { pattern, client } => {
                let url = expand_pattern(pattern, tile);
                let resp = client.get(&url).send().await?;
                if resp.status() == reqwest::StatusCode::NOT_FOUND {
                    return Ok(None);
                }
                let resp = resp.error_for_status()?;
                Ok(Some(resp.bytes().await?))
            }
            TileSourceKind::MvtFile { pattern } => {
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
/// of its `tiles` array plus its `attribution`, if any. The spec
/// allows multiple endpoints for load balancing; we pick the first
/// deterministically.
async fn load_tilejson_pattern(src: &str) -> Result<(String, Option<String>), SourceError> {
    let text = if is_url(src) {
        let resp = reqwest::get(src)
            .await
            .map_err(|e| SourceError::TileJson {
                src: src.into(),
                msg: e.to_string(),
            })?
            .error_for_status()
            .map_err(|e| SourceError::TileJson {
                src: src.into(),
                msg: e.to_string(),
            })?;
        resp.text().await.map_err(|e| SourceError::TileJson {
            src: src.into(),
            msg: e.to_string(),
        })?
    } else {
        std::fs::read_to_string(src).map_err(|e| SourceError::TileJson {
            src: src.into(),
            msg: e.to_string(),
        })?
    };
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| SourceError::TileJson {
        src: src.into(),
        msg: e.to_string(),
    })?;
    let tiles = v
        .get("tiles")
        .and_then(|t| t.as_array())
        .ok_or_else(|| SourceError::TileJson {
            src: src.into(),
            msg: "missing `tiles` array".into(),
        })?;
    let first = tiles
        .first()
        .and_then(|t| t.as_str())
        .ok_or_else(|| SourceError::TileJson {
            src: src.into(),
            msg: "`tiles[0]` is missing or not a string".into(),
        })?;
    let attribution = v
        .get("attribution")
        .and_then(|a| a.as_str())
        .map(str::to_string);
    Ok((first.to_string(), attribution))
}
