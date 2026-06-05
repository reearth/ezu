//! Per-tile raster-DEM fetch + decode + stitch.
//!
//! A [`DemSourceRegistry`] holds one [`DemSourceState`] per `sources`
//! entry in the style document. For every tile rendered, the host calls
//! [`bind_dem_sources`] which:
//!
//! 1. Fetches the centre tile and (when `neighbor_fetch` is on) the 8
//!    surrounding tiles, decoding each into a `Vec<f32>` of elevations
//!    in metres.
//! 2. Bilinear-resamples that 3×3 mosaic onto the render canvas's
//!    padded grid, so gradient ops (`hillshade`, `slope`) see
//!    continuous values across the tile seam.
//! 3. Binds the resulting [`ScalarField`] under `"tile.<source-name>"`
//!    so the `dem` source node can pick it up.
//!
//! The source `url` may be an XYZ template or a TileJSON document
//! (resolved on first use; its `attribution` is inherited when the
//! source declares none). 404s within the zoom range follow the
//! source's `on-missing` policy: `empty` leaves the source unbound for
//! this tile (the `dem` node emits zero elevation), `upsample` walks
//! up parent zooms, `error` fails the render. Requests past `max-zoom`
//! always upsample from the ancestor at `max-zoom`.
//!
//! Decoded tiles are cached unboundedly per source — a tile pyramid run
//! visits each DEM tile at most once per render pass, and the working
//! set fits comfortably in memory for the zoom ranges this is intended
//! for. Add an LRU bound here if that ever stops being true.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ezu_graph::{CanvasInfo, ScalarField, TileId};
use ezu_style::{DemSource, Document, OnMissing, SourceDecl};
use reqwest::Client;
use tokio::sync::OnceCell;

use crate::host::dem_decode::{decode_dem_tile, stitch_padded_field, upsample_subregion, DemTile};
use crate::host::tilejson::resolve_tilejson;
use crate::host::TileLoader;

#[derive(Debug, thiserror::Error)]
pub enum DemFetchError {
    #[error("source `{name}` http: {msg}")]
    Http { name: String, msg: String },
    #[error("source `{name}` decode {z}/{x}/{y}: {msg}")]
    Decode {
        name: String,
        z: u8,
        x: u32,
        y: u32,
        msg: String,
    },
    #[error("source `{name}`: tile {z}/{x}/{y} is missing (on-missing: error)")]
    Missing { name: String, z: u8, x: u32, y: u32 },
}

/// All DEM sources declared by a style, ready to fetch + bind per tile.
/// Preserves the document's source order so binding is deterministic.
pub struct DemSourceRegistry {
    sources: Vec<(String, Arc<DemSourceState>)>,
}

impl DemSourceRegistry {
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.sources.iter().map(|(n, _)| n.as_str())
    }

    /// Resolve every source's URL template (fetching TileJSON where
    /// needed) and return the *inherited* attributions: upstream
    /// metadata for sources that don't declare an explicit
    /// `attribution`. Hosts merge these with `Document::attributions`.
    pub async fn resolve_metadata(&self) -> Result<Vec<String>, DemFetchError> {
        let mut out = Vec::new();
        for (_, src) in &self.sources {
            src.template().await?;
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

/// Per-source decoded-tile cache, keyed by `(z, x, y)`. Negative
/// entries (`None`) cache known-missing tiles.
type TileCache<T> = Mutex<HashMap<(u8, u32, u32), Option<Arc<T>>>>;

/// One DEM source's runtime state: config + HTTP client + decoded-tile
/// cache (negative entries cache known-missing tiles).
struct DemSourceState {
    name: String,
    spec: DemSource,
    client: Client,
    template: OnceCell<String>,
    upstream_attribution: OnceCell<Option<String>>,
    cache: TileCache<DemTile>,
}

/// Build a registry from every `dem`-typed entry in the document's
/// `sources` block. Returns an empty registry if there are none.
pub fn build_dem_sources(doc: &Document) -> DemSourceRegistry {
    let client = Client::builder()
        .user_agent(concat!("ezu/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default();
    let mut sources = Vec::new();
    for (name, decl) in &doc.sources {
        let SourceDecl::Dem(spec) = decl else {
            continue;
        };
        sources.push((
            name.clone(),
            Arc::new(DemSourceState {
                name: name.clone(),
                spec: spec.clone(),
                client: client.clone(),
                template: OnceCell::new(),
                upstream_attribution: OnceCell::new(),
                cache: Mutex::new(HashMap::new()),
            }),
        ));
    }
    DemSourceRegistry { sources }
}

/// Fetch the DEM mosaic for every source in the registry and bind each
/// one onto `tile_loader` under the source's bare name (the style's
/// `dem` op references it via the same `source: "<name>"` field).
/// Cache hits short-circuit the HTTP round trip. A missing centre tile
/// under `on-missing: empty` (or exhausted `upsample`) skips the
/// binding — the `dem` node then emits a zero field.
pub async fn bind_dem_sources(
    tile_loader: &mut TileLoader<'_>,
    registry: &DemSourceRegistry,
    tile: TileId,
    canvas: CanvasInfo,
) -> Result<(), DemFetchError> {
    if registry.sources.is_empty() {
        return Ok(());
    }
    for (name, src) in &registry.sources {
        if let Some(field) = src.clone().build_padded(tile, canvas).await? {
            tile_loader.bind_scalar_field(name.to_string(), field);
        }
    }
    Ok(())
}

impl DemSourceState {
    async fn template(&self) -> Result<&String, DemFetchError> {
        self.template
            .get_or_try_init(|| async {
                let url = &self.spec.url;
                if url.split('?').next().is_some_and(|p| p.ends_with(".json")) {
                    let (template, attribution) = resolve_tilejson(&self.client, url)
                        .await
                        .map_err(|msg| DemFetchError::Http {
                            name: self.name.clone(),
                            msg,
                        })?;
                    let _ = self.upstream_attribution.set(attribution);
                    Ok(template)
                } else {
                    let _ = self.upstream_attribution.set(None);
                    Ok(url.clone())
                }
            })
            .await
    }

    async fn build_padded(
        self: Arc<Self>,
        tile: TileId,
        canvas: CanvasInfo,
    ) -> Result<Option<ScalarField>, DemFetchError> {
        let world = 1u32 << tile.z;
        let neighbor_fetch = self.spec.neighbor_fetch;
        // Coordinates of the 3x3 neighbourhood, with `None` slots for
        // tiles that lie outside the world (x clamps east-west by world,
        // y simply clamps).
        let mut coords: Vec<(i32, i32, u8, u32, u32)> = Vec::with_capacity(9);
        let dys: &[i32] = if neighbor_fetch { &[-1, 0, 1] } else { &[0] };
        let dxs: &[i32] = if neighbor_fetch { &[-1, 0, 1] } else { &[0] };
        for &dy in dys {
            for &dx in dxs {
                let ny = tile.y as i32 + dy;
                if ny < 0 || (ny as u32) >= world {
                    continue;
                }
                // X wraps in Web Mercator (date line).
                let nx = ((tile.x as i32 + dx).rem_euclid(world as i32)) as u32;
                coords.push((dx, dy, tile.z, nx, ny as u32));
            }
        }

        let mut grid: HashMap<(i32, i32), Arc<DemTile>> = HashMap::with_capacity(coords.len());
        for &(dx, dy, z, x, y) in &coords {
            match self.clone().fetch_tile(z, x, y).await? {
                Some(t) => {
                    grid.insert((dx, dy), t);
                }
                None if (dx, dy) == (0, 0) => {
                    // Centre missing: the policy decides between an
                    // unbound (zero-elevation) source and a hard error.
                    // Missing neighbours always edge-clamp.
                    return match self.spec.on_missing {
                        OnMissing::Error => Err(DemFetchError::Missing {
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
        let borrowed: HashMap<(i32, i32), &DemTile> =
            grid.iter().map(|(k, v)| (*k, v.as_ref())).collect();
        Ok(stitch_padded_field(
            &borrowed,
            self.spec.elevation_offset,
            tile,
            canvas,
        ))
    }

    /// Fetch one tile, honouring `max-zoom` overzoom and the
    /// `on-missing: upsample` parent walk. `Ok(None)` means the tile
    /// (and, under `upsample`, every ancestor) is missing.
    async fn fetch_tile(
        self: Arc<Self>,
        z: u8,
        x: u32,
        y: u32,
    ) -> Result<Option<Arc<DemTile>>, DemFetchError> {
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
                        Arc::new(upsample_subregion(&t, shift, x, y, ax, ay))
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

    /// Fetch + decode a tile at its native zoom. `Ok(None)` on 404;
    /// other failures are errors.
    async fn fetch_native(
        &self,
        z: u8,
        x: u32,
        y: u32,
    ) -> Result<Option<Arc<DemTile>>, DemFetchError> {
        if let Some(hit) = self.cache.lock().unwrap().get(&(z, x, y)).cloned() {
            return Ok(hit);
        }
        let url = self
            .template()
            .await?
            .replace("{z}", &z.to_string())
            .replace("{x}", &x.to_string())
            .replace("{y}", &y.to_string());
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| DemFetchError::Http {
                name: self.name.clone(),
                msg: format!("{url}: {e}"),
            })?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            self.cache.lock().unwrap().insert((z, x, y), None);
            return Ok(None);
        }
        let resp = resp.error_for_status().map_err(|e| DemFetchError::Http {
            name: self.name.clone(),
            msg: format!("{url}: {e}"),
        })?;
        let bytes = resp.bytes().await.map_err(|e| DemFetchError::Http {
            name: self.name.clone(),
            msg: format!("{url}: {e}"),
        })?;
        let decoded =
            decode_dem_tile(&bytes, self.spec.encoding, z, x, y).map_err(|e| match e {
                crate::host::dem_decode::DemDecodeError::Decode { z, x, y, msg } => {
                    DemFetchError::Decode {
                        name: self.name.clone(),
                        z,
                        x,
                        y,
                        msg,
                    }
                }
            })?;
        let tile = Arc::new(decoded);
        self.cache
            .lock()
            .unwrap()
            .insert((z, x, y), Some(tile.clone()));
        Ok(tile.into())
    }
}
