//! Pure (non-async, no HTTP) DEM decoding + 3×3 mosaic stitching.
//!
//! The HTTP-fetching path in [`dem`](super::dem) uses these primitives
//! after retrieving bytes; the wasm renderer uses them directly once
//! JS hands the raw tile bytes in. Lives outside the `http` feature
//! gate so it's available on `wasm32` without `reqwest`.

use std::collections::HashMap;

use ezu_core::coord::EARTH_CIRCUMFERENCE_M;
use ezu_graph::{CanvasInfo, GeoScale, ScalarField, TileId};
use ezu_style::DemEncoding;

/// One decoded DEM tile at the source's native pixel size.
pub struct DemTile {
    pub size: u32,
    pub elev: Vec<f32>,
}

#[derive(Debug, thiserror::Error)]
pub enum DemDecodeError {
    #[error("decode {z}/{x}/{y}: {msg}")]
    Decode { z: u8, x: u32, y: u32, msg: String },
}

/// Decode a single raster-DEM tile (PNG / WebP bytes, terrarium or
/// mapbox-rgb encoding) into a flat elevation grid. `(z, x, y)` is
/// used only for error messages.
pub fn decode_dem_tile(
    bytes: &[u8],
    encoding: DemEncoding,
    z: u8,
    x: u32,
    y: u32,
) -> Result<DemTile, DemDecodeError> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| DemDecodeError::Decode {
            z,
            x,
            y,
            msg: e.to_string(),
        })?
        .to_rgba8();
    let (w, h) = img.dimensions();
    if w != h {
        return Err(DemDecodeError::Decode {
            z,
            x,
            y,
            msg: format!("non-square tile {w}x{h}"),
        });
    }
    let mut elev = Vec::with_capacity((w * h) as usize);
    for px in img.pixels() {
        let [r, g, b, _] = px.0;
        elev.push(decode_sample(encoding, r, g, b));
    }
    Ok(DemTile { size: w, elev })
}

/// Stitch a 3×3 mosaic of decoded DEM tiles onto the canvas's padded
/// grid and return a [`ScalarField`] with geographic scaling derived
/// from `tile`'s centre latitude. `tiles` is keyed by `(dx, dy)` with
/// `(0, 0)` being the rendered tile; missing slots edge-clamp to the
/// centre. Output is geographically scaled and ready to feed
/// `hillshade` / `slope`.
pub fn stitch_padded_field(
    tiles: &HashMap<(i32, i32), &DemTile>,
    elevation_offset: f32,
    tile: TileId,
    canvas: CanvasInfo,
) -> Option<ScalarField> {
    let centre = tiles.get(&(0, 0))?;
    let (pw, ph) = canvas.padded_dims();
    let pad = canvas.pad as f32;
    // Per axis: a pixel's position within the tile is a fraction of that
    // axis' own extent.
    let tile_px_x = canvas.tile_w as f32;
    let tile_px_y = canvas.tile_h as f32;
    let dem_size = centre.size as f32;

    let mut elev = vec![0f32; (pw * ph) as usize];
    for py in 0..ph {
        let ty = (py as f32 - pad) / tile_px_y;
        let (dy_off, ty_local) = split_fraction(ty);
        for px in 0..pw {
            let tx = (px as f32 - pad) / tile_px_x;
            let (dx_off, tx_local) = split_fraction(tx);
            let sample_tile = tiles
                .get(&(dx_off, dy_off))
                .copied()
                .or_else(|| {
                    tiles
                        .get(&(dx_off.clamp(-1, 1), dy_off.clamp(-1, 1)))
                        .copied()
                })
                .unwrap_or(centre);
            // Edge-clamp inside the tile so missing neighbours
            // degrade gracefully (centre extends to fill the gap).
            let sx = (tx_local * dem_size).clamp(0.0, dem_size - 1.0001);
            let sy = (ty_local * dem_size).clamp(0.0, dem_size - 1.0001);
            let v = bilinear(&sample_tile.elev, sample_tile.size, sx, sy);
            elev[(py * pw + px) as usize] = v - elevation_offset;
        }
    }

    let lat_rad = tile_centre_lat_rad(tile);
    let world_pixels = canvas.tile_w as f64 * (1u64 << tile.z) as f64;
    let mpp = (EARTH_CIRCUMFERENCE_M * lat_rad.cos() / world_pixels) as f32;

    Some(ScalarField {
        width: pw,
        height: ph,
        values: elev.into(),
        nodata: None,
        geo_scale: Some(GeoScale {
            metres_per_pixel_x: mpp,
            metres_per_pixel_y: mpp,
        }),
    })
}

/// Bilinear-upsample the sub-rectangle of `ancestor` covered by tile
/// `(x, y)` at zoom offset `shift` levels below the ancestor's zoom.
/// Used to honour the source's `max-zoom` when a render asks for a
/// tile beyond it.
pub fn upsample_subregion(
    ancestor: &DemTile,
    shift: u8,
    x: u32,
    y: u32,
    ax: u32,
    ay: u32,
) -> DemTile {
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
        elev,
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
    (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan()
}
