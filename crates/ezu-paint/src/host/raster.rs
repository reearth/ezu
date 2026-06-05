//! Per-tile RGBA raster fetch + decode + stitch — the imagery twin of
//! [`dem`](super::dem).
//!
//! A [`RasterSourceRegistry`] holds one [`RasterSourceState`] per
//! `raster`-typed `sources` entry. For every tile rendered, the host
//! calls [`bind_raster_sources`] which fetches the 3×3 neighbourhood,
//! stitches it onto the padded canvas, and binds the result under the
//! source's bare name for the `raster` node to pick up.
//!
//! Backends: XYZ URL templates, TileJSON documents (template +
//! upstream attribution), and PMTiles archives (http or local path;
//! metadata `attribution` is inherited). 404s within the zoom range
//! follow the source's `on-missing` policy; requests past `max-zoom`
//! always upsample from the ancestor at `max-zoom`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ezu_graph::{CanvasInfo, RasterBuf, TileId};
use ezu_style::{Document, OnMissing, RasterSource, SourceDecl};
use pmtiles::{AsyncPmTilesReader, HttpBackend, MmapBackend};
use reqwest::Client;
use tokio::sync::OnceCell;

use crate::host::raster_decode::{
    decode_raster_tile, stitch_padded_raster, upsample_subregion_raster, RasterTile,
};
use crate::host::tilejson::resolve_tilejson;
use crate::host::TileLoader;

#[derive(Debug, thiserror::Error)]
pub enum RasterFetchError {
    #[error("source `{name}` http: {msg}")]
    Http { name: String, msg: String },
    #[error("source `{name}` open: {msg}")]
    Open { name: String, msg: String },
    #[error("source `{name}` decode: {msg}")]
    Decode { name: String, msg: String },
    #[error("source `{name}`: tile {z}/{x}/{y} is missing (on-missing: error)")]
    Missing { name: String, z: u8, x: u32, y: u32 },
}

/// All raster sources declared by a style, ready to fetch + bind per
/// tile. Preserves the document's source order.
pub struct RasterSourceRegistry {
    sources: Vec<(String, Arc<RasterSourceState>)>,
}

impl RasterSourceRegistry {
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.sources.iter().map(|(n, _)| n.as_str())
    }

    /// Open every source's backend (fetch TileJSON, open PMTiles) and
    /// return the *inherited* attributions: upstream metadata for
    /// sources that don't declare an explicit `attribution`. Hosts
    /// merge these with [`Document::attributions`].
    pub async fn resolve_metadata(&self) -> Result<Vec<String>, RasterFetchError> {
        let mut out = Vec::new();
        for (_, src) in &self.sources {
            src.backend().await?;
            if src.spec.attribution.is_none() {
                if let Some(Some(a)) = src.upstream_attribution.get() {
                    if !a.is_empty() && !out.contains(a) {
                        out.push(a.clone());
                    }
                }
            }
        }
        Ok(out)
    }
}

enum RasterBackend {
    Xyz { template: String },
    PmHttp(AsyncPmTilesReader<HttpBackend>),
    PmLocal(AsyncPmTilesReader<MmapBackend>),
}

/// Per-source decoded-tile cache, keyed by `(z, x, y)`. Negative
/// entries (`None`) cache known-missing tiles.
type TileCache = Mutex<HashMap<(u8, u32, u32), Option<Arc<RasterTile>>>>;

/// One raster source's runtime state: config + backend + decoded-tile
/// cache (negative entries cache known-missing tiles).
pub struct RasterSourceState {
    name: String,
    spec: RasterSource,
    client: Client,
    assets_dir: Option<PathBuf>,
    backend: OnceCell<RasterBackend>,
    upstream_attribution: OnceCell<Option<String>>,
    cache: TileCache,
}

/// Build a registry from every `raster`-typed entry in the document's
/// `sources` block. `assets_dir` resolves relative local PMTiles
/// paths. Backends open lazily on first use (or eagerly via
/// [`RasterSourceRegistry::resolve_metadata`]).
pub fn build_raster_sources(doc: &Document, assets_dir: Option<PathBuf>) -> RasterSourceRegistry {
    let client = Client::builder()
        .user_agent(concat!("ezu/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default();
    let mut sources = Vec::new();
    for (name, decl) in &doc.sources {
        let SourceDecl::Raster(spec) = decl else {
            continue;
        };
        sources.push((
            name.clone(),
            Arc::new(RasterSourceState {
                name: name.clone(),
                spec: spec.clone(),
                client: client.clone(),
                assets_dir: assets_dir.clone(),
                backend: OnceCell::new(),
                upstream_attribution: OnceCell::new(),
                cache: Mutex::new(HashMap::new()),
            }),
        ));
    }
    RasterSourceRegistry { sources }
}

/// Fetch + stitch the mosaic for every raster source and bind each one
/// onto `tile_loader` under the source's bare name. A missing centre
/// tile under `on-missing: empty`/exhausted-`upsample` skips the
/// binding (the `raster` node emits transparent pixels); under
/// `on-missing: error` it fails the render.
pub async fn bind_raster_sources(
    tile_loader: &mut TileLoader<'_>,
    registry: &RasterSourceRegistry,
    tile: TileId,
    canvas: CanvasInfo,
) -> Result<(), RasterFetchError> {
    for (name, src) in &registry.sources {
        if let Some(buf) = src.clone().build_padded(tile, canvas).await? {
            tile_loader.bind_raster(name.to_string(), buf);
        }
    }
    Ok(())
}

impl RasterSourceState {
    async fn backend(&self) -> Result<&RasterBackend, RasterFetchError> {
        self.backend
            .get_or_try_init(|| async {
                let url = &self.spec.url;
                if url.ends_with(".pmtiles") {
                    let (backend, metadata) = if is_url(url) {
                        let reader =
                            AsyncPmTilesReader::new_with_url(self.client.clone(), url.clone())
                                .await
                                .map_err(|e| RasterFetchError::Open {
                                    name: self.name.clone(),
                                    msg: format!("{url}: {e}"),
                                })?;
                        let meta = reader.get_metadata().await.ok();
                        (RasterBackend::PmHttp(reader), meta)
                    } else {
                        let path = match &self.assets_dir {
                            Some(dir) if !std::path::Path::new(url).is_absolute() => dir.join(url),
                            _ => PathBuf::from(url),
                        };
                        let reader =
                            AsyncPmTilesReader::new_with_path(&path)
                                .await
                                .map_err(|e| RasterFetchError::Open {
                                    name: self.name.clone(),
                                    msg: format!("{}: {e}", path.display()),
                                })?;
                        let meta = reader.get_metadata().await.ok();
                        (RasterBackend::PmLocal(reader), meta)
                    };
                    let attribution = metadata.and_then(|m| {
                        serde_json::from_str::<serde_json::Value>(&m)
                            .ok()?
                            .get("attribution")?
                            .as_str()
                            .map(str::to_string)
                    });
                    let _ = self.upstream_attribution.set(attribution);
                    Ok(backend)
                } else if url.split('?').next().is_some_and(|p| p.ends_with(".json")) {
                    let (template, attribution) = resolve_tilejson(&self.client, url)
                        .await
                        .map_err(|msg| RasterFetchError::Open {
                            name: self.name.clone(),
                            msg,
                        })?;
                    let _ = self.upstream_attribution.set(attribution);
                    Ok(RasterBackend::Xyz { template })
                } else {
                    let _ = self.upstream_attribution.set(None);
                    Ok(RasterBackend::Xyz {
                        template: url.clone(),
                    })
                }
            })
            .await
    }

    async fn build_padded(
        self: Arc<Self>,
        tile: TileId,
        canvas: CanvasInfo,
    ) -> Result<Option<RasterBuf>, RasterFetchError> {
        let world = 1u32 << tile.z;
        let neighbor_fetch = self.spec.neighbor_fetch;
        let mut coords: Vec<(i32, i32, u8, u32, u32)> = Vec::with_capacity(9);
        let offs: &[i32] = if neighbor_fetch { &[-1, 0, 1] } else { &[0] };
        for &dy in offs {
            for &dx in offs {
                let ny = tile.y as i32 + dy;
                if ny < 0 || (ny as u32) >= world {
                    continue;
                }
                let nx = ((tile.x as i32 + dx).rem_euclid(world as i32)) as u32;
                coords.push((dx, dy, tile.z, nx, ny as u32));
            }
        }

        let mut grid: HashMap<(i32, i32), Arc<RasterTile>> = HashMap::with_capacity(coords.len());
        for &(dx, dy, z, x, y) in &coords {
            match self.clone().fetch_tile(z, x, y).await? {
                Some(t) => {
                    grid.insert((dx, dy), t);
                }
                None if (dx, dy) == (0, 0) => {
                    // Centre missing: the policy decides between an
                    // unbound (transparent) source and a hard error.
                    // Missing neighbours always edge-clamp.
                    return match self.spec.on_missing {
                        OnMissing::Error => Err(RasterFetchError::Missing {
                            name: self.name.clone(),
                            z,
                            x,
                            y,
                        }),
                        _ => Ok(None),
                    };
                }
                None => {}
            }
        }
        let borrowed: HashMap<(i32, i32), &RasterTile> =
            grid.iter().map(|(k, v)| (*k, v.as_ref())).collect();
        Ok(stitch_padded_raster(&borrowed, canvas))
    }

    /// Fetch one tile, honouring `max-zoom` overzoom and the
    /// `on-missing: upsample` parent walk. `Ok(None)` means the tile
    /// (and, under `upsample`, every ancestor) is missing.
    async fn fetch_tile(
        self: Arc<Self>,
        z: u8,
        x: u32,
        y: u32,
    ) -> Result<Option<Arc<RasterTile>>, RasterFetchError> {
        if let Some(hit) = self.cache.lock().unwrap().get(&(z, x, y)).cloned() {
            return Ok(hit);
        }
        // Start at the source's native ceiling and, when allowed,
        // walk further up until a tile exists.
        let mut pz = self.spec.max_zoom.map_or(z, |mz| z.min(mz));
        let found = loop {
            let shift = z - pz;
            let (ax, ay) = (x >> shift, y >> shift);
            match self.fetch_native(pz, ax, ay).await? {
                Some(t) => {
                    let tile = if shift == 0 {
                        t
                    } else {
                        Arc::new(upsample_subregion_raster(&t, shift, x, y, ax, ay))
                    };
                    break Some(tile);
                }
                None if self.spec.on_missing == OnMissing::Upsample && pz > 0 => pz -= 1,
                None => break None,
            }
        };
        self.cache.lock().unwrap().insert((z, x, y), found.clone());
        Ok(found)
    }

    /// Fetch + decode a tile at its native zoom. `Ok(None)` on 404 /
    /// absent-from-archive; other failures are errors.
    async fn fetch_native(
        &self,
        z: u8,
        x: u32,
        y: u32,
    ) -> Result<Option<Arc<RasterTile>>, RasterFetchError> {
        if let Some(hit) = self.cache.lock().unwrap().get(&(z, x, y)).cloned() {
            return Ok(hit);
        }
        let bytes: Option<Vec<u8>> = match self.backend().await? {
            RasterBackend::Xyz { template } => {
                let url = template
                    .replace("{z}", &z.to_string())
                    .replace("{x}", &x.to_string())
                    .replace("{y}", &y.to_string());
                let resp =
                    self.client
                        .get(&url)
                        .send()
                        .await
                        .map_err(|e| RasterFetchError::Http {
                            name: self.name.clone(),
                            msg: format!("{url}: {e}"),
                        })?;
                if resp.status() == reqwest::StatusCode::NOT_FOUND {
                    None
                } else {
                    let resp = resp
                        .error_for_status()
                        .map_err(|e| RasterFetchError::Http {
                            name: self.name.clone(),
                            msg: format!("{url}: {e}"),
                        })?;
                    Some(
                        resp.bytes()
                            .await
                            .map_err(|e| RasterFetchError::Http {
                                name: self.name.clone(),
                                msg: format!("{url}: {e}"),
                            })?
                            .to_vec(),
                    )
                }
            }
            RasterBackend::PmHttp(r) => self.pm_tile_bytes(r, z, x, y).await?,
            RasterBackend::PmLocal(r) => self.pm_tile_bytes(r, z, x, y).await?,
        };
        let decoded = match bytes {
            Some(b) => Some(Arc::new(decode_raster_tile(&b, z, x, y).map_err(
                |msg| RasterFetchError::Decode {
                    name: self.name.clone(),
                    msg,
                },
            )?)),
            None => None,
        };
        self.cache
            .lock()
            .unwrap()
            .insert((z, x, y), decoded.clone());
        Ok(decoded)
    }

    async fn pm_tile_bytes<B: pmtiles::AsyncBackend + Sync + Send>(
        &self,
        reader: &AsyncPmTilesReader<B>,
        z: u8,
        x: u32,
        y: u32,
    ) -> Result<Option<Vec<u8>>, RasterFetchError> {
        let coord = pmtiles::TileCoord::new(z, x, y).map_err(|e| RasterFetchError::Http {
            name: self.name.clone(),
            msg: format!("bad coord {z}/{x}/{y}: {e}"),
        })?;
        let bytes =
            reader
                .get_tile_decompressed(coord)
                .await
                .map_err(|e| RasterFetchError::Http {
                    name: self.name.clone(),
                    msg: format!("pmtiles {z}/{x}/{y}: {e}"),
                })?;
        Ok(bytes.map(|b| b.to_vec()))
    }
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}
