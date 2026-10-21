//! Pure (non-async, no HTTP) RGBA raster-tile decoding + 3×3 mosaic
//! stitching — the imagery twin of [`dem_decode`](super::dem_decode).
//!
//! The HTTP-fetching path in [`raster`](super::raster) uses these
//! primitives after retrieving bytes; the wasm renderer uses them
//! directly once JS hands the raw tile bytes in. Lives outside the
//! `http` feature gate so it's available on `wasm32` without
//! `reqwest`.

use std::collections::HashMap;

use ezu_graph::{CanvasInfo, RasterBuf};
use ezu_style::Resampling;

use crate::host::decode_image_bytes;

/// One decoded raster tile at the source's native pixel size,
/// premultiplied RGBA8.
pub struct RasterTile {
    pub size: u32,
    pub pixels: Vec<u8>,
}

/// Decode a single raster tile (PNG / WebP / JPEG bytes, sniffed from
/// content) into a premultiplied RGBA8 grid. `(z, x, y)` is used only
/// for error messages.
pub fn decode_raster_tile(bytes: &[u8], z: u8, x: u32, y: u32) -> Result<RasterTile, String> {
    let buf = decode_image_bytes(bytes).map_err(|e| format!("decode {z}/{x}/{y}: {e}"))?;
    if buf.width != buf.height {
        return Err(format!(
            "decode {z}/{x}/{y}: non-square tile {}x{}",
            buf.width, buf.height
        ));
    }
    Ok(RasterTile {
        size: buf.width,
        pixels: buf.pixels,
    })
}

/// Stitch a 3×3 mosaic of decoded raster tiles onto the canvas's
/// padded grid. `tiles` is keyed by `(dx, dy)` with `(0, 0)` being the
/// rendered tile; missing slots edge-clamp to the centre so absent
/// neighbours degrade gracefully. Returns `None` when the centre tile
/// itself is missing. `resampling` picks how tile pixels that do not
/// land one to one on the canvas are sampled.
pub fn stitch_padded_raster(
    tiles: &HashMap<(i32, i32), &RasterTile>,
    canvas: CanvasInfo,
    resampling: Resampling,
) -> Option<RasterBuf> {
    let centre = tiles.get(&(0, 0))?;
    let (pw, ph) = canvas.padded_dims();
    let pad = canvas.pad as f32;
    // Each axis divides by its own extent, so the fraction of the tile a
    // pixel sits at stays correct on a canvas that is not square.
    let tile_px_x = canvas.tile_w as f32;
    let tile_px_y = canvas.tile_h as f32;

    let mut out = RasterBuf::new(pw, ph);
    match resampling {
        // Nearest sampling picks the tile pixel under each canvas pixel's
        // centre, as a GPU's nearest filter does.
        Resampling::Nearest => {
            for py in 0..ph {
                let ty = (py as f32 + 0.5 - pad) / tile_px_y;
                let (dy_off, ty_local) = split_fraction(ty);
                for px in 0..pw {
                    let tx = (px as f32 + 0.5 - pad) / tile_px_x;
                    let (dx_off, tx_local) = split_fraction(tx);
                    let sample_tile = tiles.get(&(dx_off, dy_off)).copied().unwrap_or(centre);
                    let size = sample_tile.size as f32;
                    let rgba = nearest_rgba(
                        &sample_tile.pixels,
                        sample_tile.size,
                        tx_local * size,
                        ty_local * size,
                    );
                    let i = ((py * pw + px) * 4) as usize;
                    out.pixels[i..i + 4].copy_from_slice(&rgba);
                }
            }
        }
        // Linear sampling blends the four tile pixels whose centres
        // surround the canvas pixel's centre, as a GPU's linear filter
        // does, reaching into the neighbouring tile across a seam. A
        // canvas the tile's size lands every canvas centre on a tile
        // pixel's centre and reads it unblended.
        Resampling::Linear => {
            let size = centre.size as f32;
            let present = |k: (i32, i32)| tiles.contains_key(&k);
            for py in 0..ph {
                let sy = mosaic_coord(py, pad, tile_px_y, size);
                for px in 0..pw {
                    let sx = mosaic_coord(px, pad, tile_px_x, size);
                    let cell = MosaicCell::at(sx, sy, centre.size, present);
                    let fetch = |t: MosaicTexel| {
                        let tile = tiles.get(&t.tile).copied().unwrap_or(centre);
                        let (x, y) = t.within(centre.size, tile.size);
                        let i = ((y * tile.size + x) * 4) as usize;
                        [
                            tile.pixels[i],
                            tile.pixels[i + 1],
                            tile.pixels[i + 2],
                            tile.pixels[i + 3],
                        ]
                    };
                    let rgba = blend4(cell.corners.map(fetch), cell.fx, cell.fy);
                    let i = ((py * pw + px) * 4) as usize;
                    out.pixels[i..i + 4].copy_from_slice(&rgba);
                }
            }
        }
    }
    Some(out)
}

/// Where canvas pixel `p`'s centre falls in the 3×3 mosaic along one axis,
/// in tile pixels from the centre tile's corner and measured between tile
/// pixel centres, so a whole number lands on one. Multiplying before
/// dividing keeps a canvas the tile's size exact.
#[inline]
pub(super) fn mosaic_coord(p: u32, pad: f32, tile_px: f32, size: f32) -> f32 {
    (p as f32 - pad + 0.5) * size / tile_px - 0.5
}

/// One pixel of the 3×3 mosaic: the tile holding it, keyed as the stitch
/// functions key their tiles, and its column and row inside that tile, in
/// the centre tile's pixels.
#[derive(Clone, Copy)]
pub(super) struct MosaicTexel {
    pub tile: (i32, i32),
    x: u32,
    y: u32,
}

impl MosaicTexel {
    /// The mosaic pixel `(gx, gy)`, counted from the centre tile's corner
    /// in tiles `size` pixels wide. Past the mosaic the outermost pixel is
    /// repeated.
    fn at(gx: i64, gy: i64, size: u32) -> Self {
        let axis = |g: i64| {
            let size = size as i64;
            match g.div_euclid(size) {
                ..=-2 => (-1, 0),
                2.. => (1, (size - 1) as u32),
                n => (n as i32, g.rem_euclid(size) as u32),
            }
        };
        let ((dx, x), (dy, y)) = (axis(gx), axis(gy));
        Self {
            tile: (dx, dy),
            x,
            y,
        }
    }

    /// Column and row inside a tile `tile_size` pixels wide: a neighbour
    /// of another size than the centre's reads the same fraction of
    /// itself.
    pub fn within(self, centre_size: u32, tile_size: u32) -> (u32, u32) {
        if tile_size == centre_size {
            return (self.x, self.y);
        }
        let at = |l: u32| (l as u64 * tile_size as u64 / centre_size as u64) as u32;
        (at(self.x), at(self.y))
    }
}

/// The four mosaic pixels a linear sample at `(sx, sy)` (from
/// [`mosaic_coord`]) blends, `[top-left, top-right, bottom-left,
/// bottom-right]`, and the fractions across the cell between them.
pub(super) struct MosaicCell {
    pub corners: [MosaicTexel; 4],
    pub fx: f32,
    pub fy: f32,
}

impl MosaicCell {
    /// A corner whose tile is missing (`present` says which are bound)
    /// takes the value of its partner across the cell where that partner's
    /// tile is there, so a missing neighbour stretches the edge it borders
    /// rather than blending in another tile's pixels. A sample that lands
    /// on a missing tile's pixel without blending reads the centre tile in
    /// that tile's place, as nearest sampling does.
    pub fn at(sx: f32, sy: f32, size: u32, present: impl Fn((i32, i32)) -> bool) -> Self {
        let (x0, y0) = (sx.floor(), sy.floor());
        let (fx, fy) = (sx - x0, sy - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let corner = |cx: i64, cy: i64| {
            let t = MosaicTexel::at(cx, cy, size);
            if present(t.tile) {
                return t;
            }
            // Partners only along an axis the sample actually blends.
            let ox = if fx > 0.0 { 2 * x0 + 1 - cx } else { cx };
            let oy = if fy > 0.0 { 2 * y0 + 1 - cy } else { cy };
            [(ox, cy), (cx, oy), (ox, oy)]
                .into_iter()
                .filter(|&p| p != (cx, cy))
                .map(|(px, py)| MosaicTexel::at(px, py, size))
                .find(|p| present(p.tile))
                .unwrap_or(t)
        };
        Self {
            corners: [
                corner(x0, y0),
                corner(x0 + 1, y0),
                corner(x0, y0 + 1),
                corner(x0 + 1, y0 + 1),
            ],
            fx,
            fy,
        }
    }
}

/// Bilinear blend of premultiplied RGBA8 texels `[top-left, top-right,
/// bottom-left, bottom-right]` at fractions `(fx, fy)` across the cell.
/// Blending premultiplied values channel-wise is the correct path (no
/// halos at transparent edges).
#[inline]
fn blend4(t: [[u8; 4]; 4], fx: f32, fy: f32) -> [u8; 4] {
    let mut out = [0u8; 4];
    for c in 0..4 {
        let a = t[0][c] as f32 * (1.0 - fx) + t[1][c] as f32 * fx;
        let b = t[2][c] as f32 * (1.0 - fx) + t[3][c] as f32 * fx;
        out[c] = (a * (1.0 - fy) + b * fy).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// Upsample the sub-rectangle of `ancestor` covered by tile `(x, y)` at
/// zoom offset `shift` levels below the ancestor's zoom, blending
/// neighbouring pixels or repeating the nearest one as `resampling` says.
/// Used for `max-zoom` overzoom and the `on-missing: upsample` parent
/// walk.
pub fn upsample_subregion_raster(
    ancestor: &RasterTile,
    shift: u8,
    x: u32,
    y: u32,
    ax: u32,
    ay: u32,
    resampling: Resampling,
) -> RasterTile {
    let scale = 1u32 << shift;
    let sub_size = ancestor.size as f32 / scale as f32;
    let origin_x = (x - ax * scale) as f32 * sub_size;
    let origin_y = (y - ay * scale) as f32 * sub_size;
    let out_size = ancestor.size;
    let mut pixels = Vec::with_capacity((out_size * out_size * 4) as usize);
    let ancestor_max = ancestor.size as f32 - 1.000_1;
    for py in 0..out_size {
        // Where the output pixel's centre falls in the ancestor, in its
        // pixels.
        let cy = origin_y + sub_size * (py as f32 + 0.5) / out_size as f32;
        for px in 0..out_size {
            let cx = origin_x + sub_size * (px as f32 + 0.5) / out_size as f32;
            let rgba = match resampling {
                // Pixel centres sit half a pixel in from their corners.
                Resampling::Linear => bilinear_rgba(
                    &ancestor.pixels,
                    ancestor.size,
                    (cx - 0.5).clamp(0.0, ancestor_max),
                    (cy - 0.5).clamp(0.0, ancestor_max),
                ),
                Resampling::Nearest => nearest_rgba(&ancestor.pixels, ancestor.size, cx, cy),
            };
            pixels.extend_from_slice(&rgba);
        }
    }
    RasterTile {
        size: out_size,
        pixels,
    }
}

/// The pixel of a `size`-square RGBA8 grid that covers the point
/// `(x, y)`, measured from the grid's corner in pixels; a point past an
/// edge takes the edge pixel.
#[inline]
fn nearest_rgba(pixels: &[u8], size: u32, x: f32, y: f32) -> [u8; 4] {
    let at = |v: f32| (v.max(0.0) as u32).min(size - 1);
    let i = ((at(y) * size + at(x)) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

/// Bilinear sample of premultiplied RGBA8 at fractional pixel
/// coordinates, measured between pixel centres.
#[inline]
fn bilinear_rgba(pixels: &[u8], size: u32, x: f32, y: f32) -> [u8; 4] {
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(size - 1);
    let y1 = (y0 + 1).min(size - 1);
    let at = |xx: u32, yy: u32| {
        let i = ((yy * size + xx) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    blend4(
        [at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1)],
        x - x0 as f32,
        y - y0 as f32,
    )
}

/// Split a tile-fractional coordinate `t` into a neighbour offset
/// `n ∈ {-1, 0, 1}` and a position inside that neighbour `∈ [0, 1)`.
#[inline]
fn split_fraction(t: f32) -> (i32, f32) {
    let n = t.floor() as i32;
    let local = t - n as f32;
    (n.clamp(-1, 1), local.clamp(0.0, 0.999_999))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_tile(size: u32, rgba: [u8; 4]) -> RasterTile {
        let mut pixels = Vec::with_capacity((size * size * 4) as usize);
        for _ in 0..(size * size) {
            pixels.extend_from_slice(&rgba);
        }
        RasterTile { size, pixels }
    }

    #[test]
    fn stitch_requires_centre() {
        let canvas = CanvasInfo::square(8, 2);
        assert!(stitch_padded_raster(&HashMap::new(), canvas, Resampling::Linear).is_none());
    }

    #[test]
    fn stitch_fills_pad_from_neighbours_and_clamps_missing() {
        let canvas = CanvasInfo::square(8, 2);
        let centre = solid_tile(8, [255, 0, 0, 255]);
        let left = solid_tile(8, [0, 255, 0, 255]);
        let mut tiles: HashMap<(i32, i32), &RasterTile> = HashMap::new();
        tiles.insert((0, 0), &centre);
        tiles.insert((-1, 0), &left);
        let out = stitch_padded_raster(&tiles, canvas, Resampling::Linear).unwrap();
        let px = |x: u32, y: u32| {
            let i = ((y * out.width + x) * 4) as usize;
            [
                out.pixels[i],
                out.pixels[i + 1],
                out.pixels[i + 2],
                out.pixels[i + 3],
            ]
        };
        // Left pad comes from the left neighbour; right pad edge-clamps
        // to the centre (no right neighbour bound).
        assert_eq!(px(0, 6), [0, 255, 0, 255]);
        assert_eq!(px(6, 6), [255, 0, 0, 255]);
        assert_eq!(px(out.width - 1, 6), [255, 0, 0, 255]);
    }

    #[test]
    fn upsample_quarters_an_ancestor() {
        // 2x2 ancestor: distinct quadrant colors; child (1, 1) of the
        // ancestor's 2x2 split should read mostly the bottom-right color.
        let mut pixels = vec![0u8; 2 * 2 * 4];
        let colors = [
            [10u8, 0, 0, 255],
            [0, 20, 0, 255],
            [0, 0, 30, 255],
            [40, 40, 40, 255],
        ];
        for (i, c) in colors.iter().enumerate() {
            pixels[i * 4..i * 4 + 4].copy_from_slice(c);
        }
        let ancestor = RasterTile { size: 2, pixels };
        let child = upsample_subregion_raster(&ancestor, 1, 1, 1, 0, 0, Resampling::Linear);
        assert_eq!(child.size, 2);
        // Bottom-right sample of the child leans to the [40,40,40] cell.
        let i = ((child.size + 1) * 4) as usize;
        assert!(child.pixels[i] > 20, "got {:?}", &child.pixels[i..i + 4]);
    }

    /// A 2x2 tile of four distinct colours, one per pixel.
    fn quadrants() -> RasterTile {
        let colors = [
            [10u8, 0, 0, 255],
            [0, 20, 0, 255],
            [0, 0, 30, 255],
            [40, 40, 40, 255],
        ];
        RasterTile {
            size: 2,
            pixels: colors.concat(),
        }
    }

    #[test]
    fn nearest_upsample_repeats_the_covering_pixel() {
        let ancestor = quadrants();
        // Child (1, 1) covers only the ancestor's bottom-right pixel, so
        // every pixel of it is that colour, unblended.
        let child = upsample_subregion_raster(&ancestor, 1, 1, 1, 0, 0, Resampling::Nearest);
        assert_eq!(child.pixels, [40u8, 40, 40, 255].repeat(4));
        // Child (0, 0) likewise takes the top-left pixel alone.
        let child = upsample_subregion_raster(&ancestor, 1, 0, 0, 0, 0, Resampling::Nearest);
        assert_eq!(child.pixels, [10u8, 0, 0, 255].repeat(4));
    }

    #[test]
    fn nearest_stitch_magnifies_into_hard_edged_blocks() {
        // A 2x2 tile on a 4x4 canvas: each tile pixel becomes a 2x2 block
        // of its own colour, where linear sampling blends neighbours.
        let canvas = CanvasInfo::square(4, 0);
        let tile = quadrants();
        let mut tiles: HashMap<(i32, i32), &RasterTile> = HashMap::new();
        tiles.insert((0, 0), &tile);
        let out = stitch_padded_raster(&tiles, canvas, Resampling::Nearest).unwrap();
        let px = |x: u32, y: u32| {
            let i = ((y * out.width + x) * 4) as usize;
            [
                out.pixels[i],
                out.pixels[i + 1],
                out.pixels[i + 2],
                out.pixels[i + 3],
            ]
        };
        for y in 0..4 {
            for x in 0..4 {
                let want = &tile.pixels[(((y / 2) * 2 + x / 2) * 4) as usize..][..4];
                assert_eq!(px(x, y), want, "({x}, {y})");
            }
        }
        let linear = stitch_padded_raster(&tiles, canvas, Resampling::Linear).unwrap();
        assert_ne!(linear.pixels, out.pixels);
    }

    #[test]
    fn both_samplings_agree_at_one_to_one() {
        // When the tile is as large as the canvas, each canvas pixel reads
        // one tile pixel whichever way it samples.
        let canvas = CanvasInfo::square(2, 0);
        let tile = quadrants();
        let mut tiles: HashMap<(i32, i32), &RasterTile> = HashMap::new();
        tiles.insert((0, 0), &tile);
        for r in [Resampling::Linear, Resampling::Nearest] {
            let out = stitch_padded_raster(&tiles, canvas, r).unwrap();
            assert_eq!(out.pixels, tile.pixels, "{r:?}");
        }
    }

    /// Red channel of each pixel in row `y`.
    fn red_row(out: &RasterBuf, y: u32) -> Vec<u8> {
        (0..out.width)
            .map(|x| out.pixels[((y * out.width + x) * 4) as usize])
            .collect()
    }

    #[test]
    fn linear_magnification_samples_pixel_centres() {
        // A 2 px tile, red 0 then 200, on a 4 px canvas: canvas centres sit
        // at -0.25, 0.25, 0.75 and 1.25 tile pixels from the first tile
        // pixel's centre, so the row is symmetric, with the outer pixels
        // holding the edge colour rather than one end reading a corner.
        let canvas = CanvasInfo::square(4, 0);
        let tile = RasterTile {
            size: 2,
            pixels: [[0u8, 0, 0, 255], [200, 0, 0, 255]].concat().repeat(2),
        };
        let tiles: HashMap<(i32, i32), &RasterTile> = HashMap::from([((0, 0), &tile)]);
        let out = stitch_padded_raster(&tiles, canvas, Resampling::Linear).unwrap();
        assert_eq!(red_row(&out, 0), [0, 50, 150, 200]);
    }

    #[test]
    fn linear_magnification_blends_across_a_tile_seam() {
        // Red 0 in the centre tile, 200 in its right neighbour: the last
        // canvas pixel's centre sits a quarter of a tile pixel past the
        // centre tile's edge pixel, so it takes a quarter of the
        // neighbour's, as the first pixel of the right pad takes a quarter
        // of the centre's from the other side.
        let canvas = CanvasInfo::square(4, 2);
        let centre = solid_tile(2, [0, 0, 0, 255]);
        let right = solid_tile(2, [200, 0, 0, 255]);
        let tiles: HashMap<(i32, i32), &RasterTile> =
            HashMap::from([((0, 0), &centre), ((1, 0), &right)]);
        let out = stitch_padded_raster(&tiles, canvas, Resampling::Linear).unwrap();
        let row = red_row(&out, 4);
        assert_eq!(&row[2..8], [0, 0, 0, 50, 150, 200]);
        // With no left neighbour bound, the left pad stretches the centre's
        // edge instead of blending in the far side of the tile.
        assert_eq!(&row[..2], [0, 0]);
    }
}
