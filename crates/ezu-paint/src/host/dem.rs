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
//! Decoded tiles are cached unboundedly per source — a tile pyramid run
//! visits each DEM tile at most once per render pass, and the working
//! set fits comfortably in memory for the zoom ranges this is intended
//! for. Add an LRU bound here if that ever stops being true.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ezu_graph::{CanvasInfo, ScalarField, TileId};
use ezu_style::{DemSource, Document, SourceDecl};
use reqwest::Client;

use crate::host::dem_decode::{decode_dem_tile, stitch_padded_field, upsample_subregion, DemTile};
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
    #[error("source `{name}` tile {z}/{x}/{y}: {msg}")]
    Other {
        name: String,
        z: u8,
        x: u32,
        y: u32,
        msg: String,
    },
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
}

/// One DEM source's runtime state: config + HTTP client + decoded-tile
/// cache.
struct DemSourceState {
    name: String,
    spec: DemSource,
    client: Client,
    cache: Mutex<HashMap<(u8, u32, u32), Arc<DemTile>>>,
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
                cache: Mutex::new(HashMap::new()),
            }),
        ));
    }
    DemSourceRegistry { sources }
}

/// Fetch the DEM mosaic for every source in the registry and bind each
/// one onto `tile_loader` under the source's bare name (the style's
/// `dem` op references it via the same `source: "<name>"` field).
/// Cache hits short-circuit the HTTP round trip.
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
        let field = src.clone().build_padded(tile, canvas).await?;
        tile_loader.bind_scalar_field(name.to_string(), field);
    }
    Ok(())
}

impl DemSourceState {
    async fn build_padded(
        self: Arc<Self>,
        tile: TileId,
        canvas: CanvasInfo,
    ) -> Result<ScalarField, DemFetchError> {
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
            let t = self.clone().fetch_tile(z, x, y).await?;
            grid.insert((dx, dy), t);
        }
        let borrowed: HashMap<(i32, i32), &DemTile> =
            grid.iter().map(|(k, v)| (*k, v.as_ref())).collect();
        stitch_padded_field(&borrowed, self.spec.elevation_offset, tile, canvas).ok_or_else(|| {
            DemFetchError::Other {
                name: self.name.clone(),
                z: tile.z,
                x: tile.x,
                y: tile.y,
                msg: "centre tile missing after fetch".into(),
            }
        })
    }

    async fn fetch_tile(
        self: Arc<Self>,
        z: u8,
        x: u32,
        y: u32,
    ) -> Result<Arc<DemTile>, DemFetchError> {
        if let Some(hit) = self.cache.lock().unwrap().get(&(z, x, y)).cloned() {
            return Ok(hit);
        }
        // Overzoom path: when the request is past the source's
        // max-zoom, fetch the ancestor at max-zoom and bilinear-upsample
        // the sub-rectangle this tile occupies. The upsampled tile is
        // cached under (z, x, y) so neighbour-fetch and repeat requests
        // hit the cache directly.
        if let Some(mz) = self.spec.max_zoom {
            if z > mz {
                let shift = z - mz;
                let ax = x >> shift;
                let ay = y >> shift;
                let ancestor = self.clone().fetch_native(mz, ax, ay).await?;
                let tile = Arc::new(upsample_subregion(&ancestor, shift, x, y, ax, ay));
                self.cache.lock().unwrap().insert((z, x, y), tile.clone());
                return Ok(tile);
            }
        }
        // `fetch_native` populates the cache itself before returning.
        self.fetch_native(z, x, y).await
    }

    async fn fetch_native(
        self: Arc<Self>,
        z: u8,
        x: u32,
        y: u32,
    ) -> Result<Arc<DemTile>, DemFetchError> {
        if let Some(hit) = self.cache.lock().unwrap().get(&(z, x, y)).cloned() {
            return Ok(hit);
        }
        let url = self
            .spec
            .url
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
            })?
            .error_for_status()
            .map_err(|e| DemFetchError::Http {
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
        self.cache.lock().unwrap().insert((z, x, y), tile.clone());
        Ok(tile)
    }
}
