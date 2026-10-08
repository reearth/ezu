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
    /// Deepest zoom the source itself claims to hold (TileJSON
    /// `maxzoom`, PMTiles header `max_zoom`). `None` for a bare
    /// `{z}/{x}/{y}` template, which carries no such metadata.
    data_maxzoom: Option<u8>,
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
                let (kind, metadata, data_maxzoom) = if is_url(arg) {
                    let client = Client::new();
                    let reader = AsyncPmTilesReader::new_with_url(client, arg)
                        .await
                        .map_err(|e| SourceError::PmTilesOpen(e.to_string()))?;
                    let meta = reader.get_metadata().await.ok();
                    let maxzoom = reader.get_header().max_zoom;
                    (
                        TileSourceKind::PmTilesHttp(Arc::new(reader)),
                        meta,
                        Some(maxzoom),
                    )
                } else {
                    let reader = AsyncPmTilesReader::new_with_path(arg)
                        .await
                        .map_err(|e| SourceError::PmTilesOpen(e.to_string()))?;
                    let meta = reader.get_metadata().await.ok();
                    let maxzoom = reader.get_header().max_zoom;
                    (
                        TileSourceKind::PmTilesLocal(Arc::new(reader)),
                        meta,
                        Some(maxzoom),
                    )
                };
                let attribution = metadata.and_then(|m| {
                    serde_json::from_str::<serde_json::Value>(&m)
                        .ok()?
                        .get("attribution")?
                        .as_str()
                        .map(str::to_string)
                });
                Ok(Self {
                    kind,
                    attribution,
                    data_maxzoom,
                })
            }
            SourceSpec::Mvt(arg) => {
                let (pattern, attribution, data_maxzoom) = if looks_like_tilejson(arg) {
                    let (resolved, attribution, maxzoom) = load_tilejson_pattern(arg).await?;
                    tracing::info!("tilejson {arg} → {resolved}");
                    (resolved, attribution, maxzoom)
                } else {
                    (arg.clone(), None, None)
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
                Ok(Self {
                    kind,
                    attribution,
                    data_maxzoom,
                })
            }
        }
    }

    /// Deepest zoom the source claims to hold, when it says so. Hosts
    /// use it as the base for how far past the data they can still
    /// render by reprojecting ancestor tiles.
    pub fn data_maxzoom(&self) -> Option<u8> {
        self.data_maxzoom
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

/// One `mvt` / `pmtiles` source the style declares, opened, with the zoom
/// range it serves. Its layers bind under its own name, which is what the
/// style's `features` nodes reference.
pub struct FeatureSource {
    pub name: Arc<str>,
    tiles: TileSource,
    min_zoom: Option<u8>,
    max_zoom: Option<u8>,
}

impl FeatureSource {
    /// The deepest zoom the source serves: its declared `max-zoom`, else
    /// what the source itself claims (TileJSON `maxzoom`, PMTiles header).
    pub fn max_zoom(&self) -> Option<u8> {
        self.max_zoom.or(self.tiles.data_maxzoom())
    }

    /// Upstream attribution captured when the source was opened.
    pub fn attribution(&self) -> Option<&str> {
        self.tiles.attribution()
    }

    /// The tile to fetch in order to draw `tile`: `None` below the
    /// source's `min-zoom`, where it has nothing to show; the covering
    /// ancestor past its [`max_zoom`](Self::max_zoom); else `tile` itself.
    pub fn native_tile(&self, tile: TileId) -> Option<TileId> {
        if self.min_zoom.is_some_and(|mz| tile.z < mz) {
            return None;
        }
        Some(
            self.max_zoom()
                .and_then(|mz| tile.ancestor_at(mz))
                .unwrap_or(tile),
        )
    }

    /// Fetch exactly `tile`, with no zoom range applied and no fallback.
    pub async fn fetch_tile(&self, tile: TileId) -> Result<Option<Bytes>, SourceError> {
        self.tiles.fetch(tile).await
    }

    /// Fetch what draws `tile`: its [`native_tile`](Self::native_tile),
    /// walking up `max_parent_levels` more on a miss. Returns the bytes
    /// with the tile they came from, for the caller to clip into `tile`.
    pub async fn fetch(
        &self,
        tile: TileId,
        max_parent_levels: u8,
    ) -> Result<Option<(Bytes, TileId)>, SourceError> {
        match self.native_tile(tile) {
            Some(native) => {
                self.tiles
                    .fetch_with_fallback(native, max_parent_levels)
                    .await
            }
            None => Ok(None),
        }
    }

    /// Fetch the neighbours of `tile` at `offsets` for cross-tile
    /// placement, each through [`fetch`](Self::fetch) so it resolves
    /// against its own ancestor. `x` wraps at the antimeridian, rows past
    /// the poles are skipped, and a missing neighbour is left out (never
    /// an error).
    pub async fn fetch_neighbors(
        &self,
        tile: TileId,
        offsets: &[(i32, i32)],
        max_parent_levels: u8,
    ) -> Result<Vec<((i32, i32), (Bytes, TileId))>, SourceError> {
        let world = 1i64 << tile.z;
        let mut out = Vec::with_capacity(offsets.len());
        for &(dx, dy) in offsets {
            let ny = tile.y as i64 + dy as i64;
            if ny < 0 || ny >= world {
                continue;
            }
            let nx = (tile.x as i64 + dx as i64).rem_euclid(world) as u32;
            let ntile = TileId::new(tile.z, nx, ny as u32);
            if let Some(hit) = self.fetch(ntile, max_parent_levels).await? {
                out.push(((dx, dy), hit));
            }
        }
        Ok(out)
    }
}

/// Every `mvt` / `pmtiles` source a style declares, in declaration order.
#[derive(Default)]
pub struct FeatureSources(Vec<FeatureSource>);

impl FeatureSources {
    /// Open each feature source `doc` declares. `cli_override` (a
    /// `--mvt` / `--pmtiles` flag and its name) replaces the URL of the
    /// first one, keeping its name and zoom range; the style must declare
    /// at least one for it to stand in for.
    pub async fn open(
        doc: &ezu::style::Document,
        cli_override: Option<(SourceSpec, &'static str)>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        use ezu::style::SourceDecl;
        let mut cli_override = cli_override;
        let mut out = Vec::new();
        for (name, decl) in &doc.sources {
            let (spec, origin) = match decl {
                SourceDecl::Mvt(s) => (SourceSpec::Mvt(s.url.clone()), "style sources (mvt)"),
                SourceDecl::Pmtiles(s) => (
                    SourceSpec::PmTiles(s.url.clone()),
                    "style sources (pmtiles)",
                ),
                _ => continue,
            };
            let (spec, origin) = cli_override.take().unwrap_or((spec, origin));
            tracing::info!("opening source `{name}` ({origin}): {spec:?}");
            out.push(FeatureSource {
                name: Arc::from(name.as_str()),
                tiles: TileSource::open(&spec).await?,
                min_zoom: decl.min_zoom(),
                max_zoom: decl.max_zoom(),
            });
        }
        if let Some((spec, origin)) = cli_override {
            return Err(format!(
                "{origin} ({spec:?}) requires the style to declare a matching `mvt`/`pmtiles` source, but the document has none — `features` nodes have no source to reference"
            )
            .into());
        }
        Ok(Self(out))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &FeatureSource> {
        self.0.iter()
    }

    /// The source named `name`, or the first one when `name` is `None`.
    pub fn get(&self, name: Option<&str>) -> Option<&FeatureSource> {
        match name {
            Some(name) => self.0.iter().find(|s| &*s.name == name),
            None => self.0.first(),
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
/// of its `tiles` array plus its `attribution` and `maxzoom`, if any.
/// The spec allows multiple endpoints for load balancing; we pick the
/// first deterministically.
async fn load_tilejson_pattern(
    src: &str,
) -> Result<(String, Option<String>, Option<u8>), SourceError> {
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
    let maxzoom = v
        .get("maxzoom")
        .and_then(serde_json::Value::as_u64)
        .and_then(|z| u8::try_from(z).ok());
    Ok((first.to_string(), attribution, maxzoom))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two `{z}/{x}/{y}` directories with different zoom ranges, as a
    /// translated MapLibre style with one source per data set declares
    /// them, and the document naming both.
    fn two_sources(root: &std::path::Path) -> ezu::style::Document {
        let tile = |dir: &str, z: u8, x: u32, y: u32, body: &str| {
            let d = root.join(dir).join(z.to_string()).join(x.to_string());
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(format!("{y}.pbf")), body).unwrap();
        };
        tile("blocks", 14, 4, 8, "blocks 14");
        tile("buildings", 17, 32, 64, "buildings 17");
        tile("buildings", 18, 64, 128, "buildings 18");
        let url = |dir: &str| format!("{}/{dir}/{{z}}/{{x}}/{{y}}.pbf", root.display());
        ezu::style::Document::from_json(&format!(
            r##"{{
              "name": "two",
              "sources": {{
                "blocks": {{ "type": "mvt", "url": "{}", "min-zoom": 14, "max-zoom": 14 }},
                "buildings": {{ "type": "mvt", "url": "{}", "max-zoom": 17 }}
              }},
              "nodes": {{ "out": {{ "op": "solid", "color": "#000000" }} }},
              "output": "@out"
            }}"##,
            url("blocks"),
            url("buildings"),
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn every_feature_source_is_fetched_at_its_own_zoom() {
        let root = std::env::temp_dir().join(format!("ezu-feature-sources-{}", std::process::id()));
        let doc = two_sources(&root);
        let sources = FeatureSources::open(&doc, None).await.unwrap();
        let names: Vec<&str> = sources.iter().map(|s| &*s.name).collect();
        assert_eq!(names, ["blocks", "buildings"]);

        let fetch = |name: &str, tile: TileId| {
            let src = sources.get(Some(name)).unwrap();
            async move {
                src.fetch(tile, 0)
                    .await
                    .unwrap()
                    .map(|(b, from)| (String::from_utf8(b.to_vec()).unwrap(), from))
            }
        };
        let z18 = TileId::new(18, 64, 128);
        // Past each source's max-zoom the covering ancestor is drawn from,
        // even where a deeper tile exists.
        assert_eq!(
            fetch("blocks", z18).await,
            Some(("blocks 14".into(), TileId::new(14, 4, 8)))
        );
        assert_eq!(
            fetch("buildings", z18).await,
            Some(("buildings 17".into(), TileId::new(17, 32, 64)))
        );
        // Below its min-zoom a source brings nothing.
        assert_eq!(fetch("blocks", TileId::new(13, 2, 4)).await, None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_cli_source_stands_in_for_the_first_and_needs_one() {
        let root =
            std::env::temp_dir().join(format!("ezu-feature-override-{}", std::process::id()));
        let doc = two_sources(&root);
        let flag = format!("{}/buildings/{{z}}/{{x}}/{{y}}.pbf", root.display());
        let sources =
            FeatureSources::open(&doc, Some((SourceSpec::Mvt(flag.clone()), "--mvt flag")))
                .await
                .unwrap();
        // `blocks` now reads the flag's directory, keeping its own range.
        let blocks = sources.get(Some("blocks")).unwrap();
        assert_eq!(
            blocks.native_tile(TileId::new(18, 64, 128)),
            Some(TileId::new(14, 4, 8))
        );
        assert!(blocks
            .fetch(TileId::new(14, 4, 8), 0)
            .await
            .unwrap()
            .is_none());

        let none = ezu::style::Document::from_json(
            r##"{ "name": "none", "nodes": { "out": { "op": "solid", "color": "#000000" } }, "output": "@out" }"##,
        )
        .unwrap();
        assert!(
            FeatureSources::open(&none, Some((SourceSpec::Mvt(flag), "--mvt flag")))
                .await
                .is_err()
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
