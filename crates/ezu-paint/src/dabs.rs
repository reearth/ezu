//! Hokusai-backed dab scatter fill.
//!
//! Each candidate position is computed from a **world-space deterministic
//! seed**, so the same world coordinate emits the same dab regardless of
//! which tile is being rendered. That is what keeps polygon fills seamless
//! across tile boundaries.

use ezu_core::{
    seed::{next_unit, world_seed},
    TileId, WorldPos,
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

/// Salt for the world seed used by dab scatter; lets other consumers
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

    let spacing = style.spacing_px.max(0.5);
    let axis_tiles = (1u64 << tile.z) as f64;
    let world_origin_x = tile.x as f64 / axis_tiles;
    let world_origin_y = tile.y as f64 / axis_tiles;
    // World coord per tile pixel (square tiles).
    let world_per_px = 1.0 / (axis_tiles * tile_w as f64);

    let cols = (pw as f32 / spacing).ceil() as u32 + 1;
    let rows = (ph as f32 / spacing).ceil() as u32 + 1;

    for row in 0..rows {
        for col in 0..cols {
            let cell_px_x = col as f32 * spacing; // in padded canvas space
            let cell_px_y = row as f32 * spacing;

            // Padded canvas pixel (cell_px) corresponds to tile pixel
            // (cell_px - pad). World coord is anchored at tile origin.
            let wx = world_origin_x + (cell_px_x as f64 - pad as f64) * world_per_px;
            let wy = world_origin_y + (cell_px_y as f64 - pad as f64) * world_per_px;

            let mut state = world_seed(WorldPos::new(wx, wy), DAB_SCATTER_SALT);

            let jx = (next_unit(&mut state) - 0.5) * spacing * style.position_jitter;
            let jy = (next_unit(&mut state) - 0.5) * spacing * style.position_jitter;
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
