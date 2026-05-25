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
//! 3. Binds the resulting [`HeightField`] under `"tile.<source-name>"`
//!    so the `dem` source node can pick it up.
//!
//! Decoded tiles are cached unboundedly per source — a tile pyramid run
//! visits each DEM tile at most once per render pass, and the working
//! set fits comfortably in memory for the zoom ranges this is intended
//! for. Add an LRU bound here if that ever stops being true.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ezu_graph::{CanvasInfo, HeightField, TileId};
use ezu_style::{DemEncoding, DemSource, Document, SourceDecl};
use reqwest::Client;

use crate::host::TileLoader;

const EARTH_CIRCUMFERENCE_M: f64 = 40_075_016.685_578_488;

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

/// One decoded DEM tile at the source's native pixel size.
struct DemTile {
    size: u32,
    elev: Arc<[f32]>,
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
        let SourceDecl::Dem(spec) = decl;
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
/// one onto `tile_loader` under `"tile.<source-name>"`. Cache hits
/// short-circuit the HTTP round trip.
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
        tile_loader.bind_height_field(format!("tile.{name}"), field);
    }
    Ok(())
}

impl DemSourceState {
    async fn build_padded(
        self: Arc<Self>,
        tile: TileId,
        canvas: CanvasInfo,
    ) -> Result<HeightField, DemFetchError> {
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
            let tile = self.clone().fetch_tile(z, x, y).await?;
            grid.insert((dx, dy), tile);
        }
        let centre = grid
            .get(&(0, 0))
            .cloned()
            .expect("centre tile is always fetched");

        let padded_size = canvas.padded_size();
        let pad = canvas.pad as f32;
        let tile_px = canvas.tile_size as f32;
        let dem_size = centre.size as f32;
        let offset = self.spec.elevation_offset;

        let mut elev = vec![0f32; (padded_size * padded_size) as usize];
        for py in 0..padded_size {
            // Tile-fractional Y from the centre tile's top edge.
            let ty = (py as f32 - pad) / tile_px;
            // 3x3 grid Y: 0..1 → centre, <0 → north, >=1 → south.
            let (dy_off, ty_local) = split_fraction(ty);
            for px in 0..padded_size {
                let tx = (px as f32 - pad) / tile_px;
                let (dx_off, tx_local) = split_fraction(tx);
                let sample_tile = grid
                    .get(&(dx_off, dy_off))
                    .or_else(|| grid.get(&(dx_off.clamp(-1, 1), dy_off.clamp(-1, 1))))
                    .unwrap_or(&centre);
                // Local DEM pixel coordinate, clamped to a sample
                // inside the tile so missing neighbours degrade to
                // edge-clamp from the centre.
                let sx = (tx_local * dem_size).clamp(0.0, dem_size - 1.0001);
                let sy = (ty_local * dem_size).clamp(0.0, dem_size - 1.0001);
                let v = bilinear(&sample_tile.elev, sample_tile.size, sx, sy);
                elev[(py * padded_size + px) as usize] = v - offset;
            }
        }

        // metres-per-pixel of the padded grid, derived from the tile's
        // centre latitude. Web Mercator pixel pitch is latitude-
        // dependent; for hillshade purposes a single scale taken at
        // tile centre is close enough.
        let lat_rad = tile_centre_lat_rad(tile);
        let world_pixels = canvas.tile_size as f64 * (1u64 << tile.z) as f64;
        let mpp = (EARTH_CIRCUMFERENCE_M * lat_rad.cos() / world_pixels) as f32;

        Ok(HeightField {
            width: padded_size,
            height: padded_size,
            metres_per_pixel_x: mpp,
            metres_per_pixel_y: mpp,
            elev: elev.into(),
            nodata: None,
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
        let img = image::load_from_memory(&bytes)
            .map_err(|e| DemFetchError::Decode {
                name: self.name.clone(),
                z,
                x,
                y,
                msg: e.to_string(),
            })?
            .to_rgba8();
        let (w, h) = img.dimensions();
        if w != h {
            return Err(DemFetchError::Decode {
                name: self.name.clone(),
                z,
                x,
                y,
                msg: format!("non-square tile {w}x{h}"),
            });
        }
        let mut elev = Vec::with_capacity((w * h) as usize);
        for px in img.pixels() {
            let [r, g, b, _] = px.0;
            elev.push(decode_sample(self.spec.encoding, r, g, b));
        }
        let tile = Arc::new(DemTile {
            size: w,
            elev: elev.into(),
        });
        self.cache.lock().unwrap().insert((z, x, y), tile.clone());
        Ok(tile)
    }
}

/// Bilinear-upsample the sub-rectangle of `ancestor` covered by tile
/// `(x, y)` at zoom offset `shift` levels below the ancestor's zoom.
///
/// `shift = z - ancestor_z`; the requested tile occupies a
/// `(1 / 2^shift)` square inside the ancestor at offset
/// `(x - ax * 2^shift, y - ay * 2^shift)` (in ancestor sub-tile units).
/// Output preserves the ancestor's pixel count so consumers keep getting
/// a familiar tile size — gradient ops just see a smoother surface than
/// the source actually provides.
fn upsample_subregion(ancestor: &DemTile, shift: u8, x: u32, y: u32, ax: u32, ay: u32) -> DemTile {
    let scale = 1u32 << shift;
    let sub_size = ancestor.size as f32 / scale as f32;
    let origin_x = (x - ax * scale) as f32 * sub_size;
    let origin_y = (y - ay * scale) as f32 * sub_size;
    let out_size = ancestor.size;
    let mut elev = Vec::with_capacity((out_size * out_size) as usize);
    let ancestor_max = ancestor.size as f32 - 1.000_1;
    for py in 0..out_size {
        let sy = (origin_y + sub_size * (py as f32 + 0.5) / out_size as f32 - 0.5)
            .clamp(0.0, ancestor_max);
        for px in 0..out_size {
            let sx = (origin_x + sub_size * (px as f32 + 0.5) / out_size as f32 - 0.5)
                .clamp(0.0, ancestor_max);
            elev.push(bilinear(&ancestor.elev, ancestor.size, sx, sy));
        }
    }
    DemTile {
        size: out_size,
        elev: elev.into(),
    }
}

#[inline]
fn decode_sample(enc: DemEncoding, r: u8, g: u8, b: u8) -> f32 {
    match enc {
        DemEncoding::Terrarium => (r as f32) * 256.0 + (g as f32) + (b as f32) / 256.0 - 32768.0,
        DemEncoding::MapboxRgb => {
            -10000.0 + ((r as f32) * 65536.0 + (g as f32) * 256.0 + (b as f32)) * 0.1
        }
    }
}

#[inline]
fn bilinear(elev: &[f32], size: u32, x: f32, y: f32) -> f32 {
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(size - 1);
    let y1 = (y0 + 1).min(size - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let i00 = (y0 * size + x0) as usize;
    let i10 = (y0 * size + x1) as usize;
    let i01 = (y1 * size + x0) as usize;
    let i11 = (y1 * size + x1) as usize;
    let a = elev[i00] * (1.0 - fx) + elev[i10] * fx;
    let b = elev[i01] * (1.0 - fx) + elev[i11] * fx;
    a * (1.0 - fy) + b * fy
}

/// Split a tile-fractional coordinate `t` (in tile units from the centre
/// tile's origin) into a neighbour offset `n ∈ {-1, 0, 1}` and a
/// position inside that neighbour `∈ [0, 1)`.
#[inline]
fn split_fraction(t: f32) -> (i32, f32) {
    let n = t.floor() as i32;
    let local = t - n as f32;
    (n.clamp(-1, 1), local.clamp(0.0, 0.999_999))
}

/// Latitude (radians) of a Web Mercator tile's vertical centre.
fn tile_centre_lat_rad(tile: TileId) -> f64 {
    let n = (1u64 << tile.z) as f64;
    let y = tile.y as f64 + 0.5;
    let lat_rad = (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan();
    lat_rad
}
