//! Hokusai-backed dab scatter fill for polygons.
//!
//! The candidate lattice is anchored in **world space**, not on the tile
//! being drawn: cells are indexed off the global pixel grid at this zoom, and
//! each cell's jitter comes from that integer index alone. Two tiles meeting
//! at a border therefore agree on where the cells are *and* on how each one
//! jittered, which is what keeps polygon fills seamless.
//!
//! Anchoring on the canvas instead — counting cells from the tile's own
//! corner — would only line up when `spacing_px` divided the tile width, and
//! step the pattern by a fraction of a cell at every border otherwise.

use ezu_core::{
    seed::{cell_seed, next_unit},
    TileId,
};
use ezu_features::Polygon;
use hokusai::color::RgbaF32;
use hokusai::tile_mem::MemSurface;
use hokusai::{Dab, TiledSurface};
use tiny_skia::{Color, FillRule, Paint, PixmapPaint, Transform};

use crate::{build_polygon_path, Canvas};

/// Style for a hokusai scatter-dab fill pass.
#[derive(Debug, Clone)]
pub struct DabFillStyle {
    /// Linear sRGB color used for every dab (jitter is applied to value).
    pub color: RgbaF32,
    /// Base opacity per dab (0..1).
    pub opacity: f32,
    /// Base dab radius in canvas pixels.
    pub radius_px: f32,
    /// Brush hardness (0..1).
    pub hardness: f32,
    /// Pigment-mixing factor (0..1). >0 enables libmypaint's spectral mode.
    pub paint: f32,
    /// Average distance between dab candidates, in canvas pixels.
    pub spacing_px: f32,
    /// Position jitter as a fraction of `spacing_px` (0 = grid, 1 = full cell).
    pub position_jitter: f32,
    /// Multiplicative radius jitter (e.g. 0.3 = ±30%).
    pub size_jitter: f32,
    /// Multiplicative opacity jitter (e.g. 0.3 = ±30%).
    pub opacity_jitter: f32,
    /// Value (brightness) jitter applied to the color in linear sRGB.
    pub value_jitter: f32,
}

impl Default for DabFillStyle {
    fn default() -> Self {
        Self {
            color: RgbaF32::new(0.34, 0.46, 0.62, 1.0),
            opacity: 0.18,
            radius_px: 6.0,
            hardness: 0.55,
            paint: 1.0,
            spacing_px: 4.0,
            position_jitter: 0.9,
            size_jitter: 0.35,
            opacity_jitter: 0.25,
            value_jitter: 0.08,
        }
    }
}

/// Salt for the lattice seed used by dab scatter; lets other consumers
/// (e.g. paper noise, edge stroking) derive uncorrelated sequences.
pub const DAB_SCATTER_SALT: u32 = 0xE2_70_DA_B5;

/// Paint polygons via scattered hokusai dabs with world-deterministic jitter,
/// flatten the result, and composite over `canvas`.
pub fn paint_polygons_dabs(
    canvas: &mut Canvas,
    polygons: &[Polygon],
    extent: u32,
    tile: TileId,
    style: &DabFillStyle,
) {
    let pw = canvas.width();
    let ph = canvas.height();
    if pw == 0 || ph == 0 || polygons.is_empty() {
        return;
    }

    let tile_w = canvas.tile_width();
    let pad = canvas.pad();
    let mask = rasterize_mask(polygons, extent, canvas);
    let mut surface = MemSurface::new();

    let spacing = style.spacing_px.max(0.5) as f64;
    let tile_h = canvas.tile_height();
    // The tile's top-left corner on the global pixel grid at this zoom. Cell
    // indices are taken there, so they name the same cell from whichever tile
    // is asking.
    let origin_px_x = tile.x as f64 * tile_w as f64;
    let origin_px_y = tile.y as f64 * tile_h as f64;
    // Padded canvas pixel `p` is global pixel `origin + p - pad`, so the
    // canvas spans `[origin - pad, origin + pw - pad]`. Every cell meeting
    // that span can put a dab in it.
    let i0 = ((origin_px_x - pad as f64) / spacing).floor() as i64;
    let i1 = ((origin_px_x + pw as f64 - pad as f64) / spacing).floor() as i64;
    let j0 = ((origin_px_y - pad as f64) / spacing).floor() as i64;
    let j1 = ((origin_px_y + ph as f64 - pad as f64) / spacing).floor() as i64;

    for j in j0..=j1 {
        for i in i0..=i1 {
            // Back into this canvas's padded pixel frame.
            let cell_px_x = (i as f64 * spacing - origin_px_x + pad as f64) as f32;
            let cell_px_y = (j as f64 * spacing - origin_px_y + pad as f64) as f32;

            // Seeded from the integer cell index alone: no dependence on the
            // tile being drawn, on iteration order, or on any float world
            // position that two tiles might round differently.
            let mut state = cell_seed(i, j, DAB_SCATTER_SALT);

            // Fixed draw order: x offset, y offset, then size, opacity, value.
            let spacing_px = spacing as f32;
            let jx = (next_unit(&mut state) - 0.5) * spacing_px * style.position_jitter;
            let jy = (next_unit(&mut state) - 0.5) * spacing_px * style.position_jitter;
            let dab_x = cell_px_x + jx;
            let dab_y = cell_px_y + jy;

            if dab_x < 0.0 || dab_y < 0.0 || dab_x >= pw as f32 || dab_y >= ph as f32 {
                continue;
            }
            let ix = dab_x as u32;
            let iy = dab_y as u32;
            if !mask[(iy * pw + ix) as usize] {
                continue;
            }

            let size_mult = 1.0 + (next_unit(&mut state) - 0.5) * 2.0 * style.size_jitter;
            let opacity_mult = 1.0 + (next_unit(&mut state) - 0.5) * 2.0 * style.opacity_jitter;
            let value_jit = (next_unit(&mut state) - 0.5) * 2.0 * style.value_jitter;

            let dab = Dab {
                x: dab_x,
                y: dab_y,
                radius: (style.radius_px * size_mult).max(0.5),
                color: RgbaF32 {
                    r: (style.color.r + value_jit).clamp(0.0, 1.0),
                    g: (style.color.g + value_jit).clamp(0.0, 1.0),
                    b: (style.color.b + value_jit).clamp(0.0, 1.0),
                    a: 1.0,
                },
                opaque: (style.opacity * opacity_mult).clamp(0.0, 1.0),
                hardness: style.hardness,
                alpha_eraser: 1.0,
                aspect_ratio: 1.0,
                angle: 0.0,
                lock_alpha: 0.0,
                colorize: 0.0,
                posterize: 0.0,
                posterize_num: 0.0,
                paint: style.paint,
                anti_aliasing: 1.0,
            };
            surface.draw_dab(&dab);
        }
    }

    let pixmap = hokusai::tiny_skia::flatten_transparent(&surface, pw, ph);
    canvas.pixmap_mut().draw_pixmap(
        0,
        0,
        pixmap.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
}

/// Rasterize the union of polygons into a binary mask at padded canvas resolution.
fn rasterize_mask(polygons: &[Polygon], extent: u32, canvas: &Canvas) -> Vec<bool> {
    let pw = canvas.width();
    let ph = canvas.height();
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph).expect("non-zero mask size");
    let mut paint = Paint::default();
    paint.set_color(Color::WHITE);
    paint.anti_alias = false;

    let sx = canvas.tile_width() as f32 / extent as f32;
    let sy = canvas.tile_height() as f32 / extent as f32;
    let ox = canvas.pad() as f32;
    let oy = canvas.pad() as f32;
    for poly in polygons {
        if let Some(path) = build_polygon_path(poly, sx, sy, ox, oy) {
            pixmap.fill_path(
                &path,
                &paint,
                FillRule::EvenOdd,
                Transform::identity(),
                None,
            );
        }
    }

    pixmap.pixels().iter().map(|p| p.alpha() > 0).collect()
}
