//! Polyline watercolor stroking via `hokusai::Brush::stroke_to`.
//!
//! Each polyline (e.g. an MVT road feature) is walked vertex-by-vertex,
//! emitting a hokusai stroke event per point. Pressure is jittered using a
//! world-deterministic seed so the same world coordinate produces the same
//! pressure regardless of which tile is being rendered — the line's
//! character is preserved across tile boundaries.

use ezu_core::{seed::world_seed, TileId, WorldPos};
use hokusai::tile_mem::MemSurface;
use hokusai::{Brush, BrushSetting, BrushState};
use tiny_skia::{PixmapPaint, Transform};

use crate::Canvas;

/// Salt for world-seeded pressure / dtime jitter along strokes.
pub const LINE_STROKE_SALT: u32 = 0xEB_E2_C0_1E;

/// Style for a watercolor line stroke pass.
#[derive(Debug, Clone)]
pub struct LineStrokeStyle {
    /// Linear-sRGB color in `[0, 1]`. Written into the brush's `color_*`
    /// base settings, so the brush's own color is overridden.
    pub color: [f32; 3],
    /// Base pressure in `[0, 1]` for every emitted event.
    pub pressure_base: f32,
    /// Multiplicative jitter (e.g. `0.2` → ±20 %) applied per vertex.
    pub pressure_jitter: f32,
    /// `dtime` between successive vertex events, in seconds. Controls how
    /// dynamics-driven brushes interpret stroke speed.
    pub dtime: f32,
}

impl Default for LineStrokeStyle {
    fn default() -> Self {
        Self {
            color: [0.18, 0.13, 0.10],
            pressure_base: 0.7,
            pressure_jitter: 0.2,
            dtime: 0.02,
        }
    }
}

/// Stroke a collection of polylines onto `canvas` using `brush`.
///
/// Lines are in MVT tile-local coordinates (`[0, extent]`, y-down). The brush
/// is cloned so the call is non-destructive; its `color_h/s/v` are replaced by
/// `style.color`.
pub fn paint_lines(
    canvas: &mut Canvas,
    lines: &[Vec<(i32, i32)>],
    extent: u32,
    tile: TileId,
    brush: &Brush,
    style: &LineStrokeStyle,
) {
    let pw = canvas.width();
    let ph = canvas.height();
    let tile_w = canvas.tile_width();
    let pad = canvas.pad() as f32;
    if pw == 0 || ph == 0 || lines.is_empty() {
        return;
    }

    let mut brush = brush.clone();
    let (hue, sat, val) = linear_rgb_to_hsv(style.color);
    brush.get_mut(BrushSetting::ColorH).base_value = hue;
    brush.get_mut(BrushSetting::ColorS).base_value = sat;
    brush.get_mut(BrushSetting::ColorV).base_value = val;

    let mut surface = MemSurface::new();
    let sx = tile_w as f32 / extent as f32;
    let sy = canvas.tile_height() as f32 / extent as f32;

    let axis_tiles = (1u64 << tile.z) as f64;
    let world_origin_x = tile.x as f64 / axis_tiles;
    let world_origin_y = tile.y as f64 / axis_tiles;
    let world_per_px = 1.0 / (axis_tiles * tile_w as f64);

    for line in lines {
        if line.len() < 2 {
            continue;
        }
        let mut state = BrushState::default();
        let mut first = true;
        for &(x, y) in line {
            // Padded canvas coords (tile-local px + pad).
            let px = x as f32 * sx + pad;
            let py = y as f32 * sy + pad;
            // World coord is anchored at tile origin (subtract pad).
            let wx = world_origin_x + (px as f64 - pad as f64) * world_per_px;
            let wy = world_origin_y + (py as f64 - pad as f64) * world_per_px;

            let mut seed = world_seed(WorldPos::new(wx, wy), LINE_STROKE_SALT);
            let pj = (next_unit(&mut seed) - 0.5) * 2.0 * style.pressure_jitter;
            let pressure = (style.pressure_base + pj).clamp(0.0, 1.0);

            // First event of each line: dtime > 5 → libmypaint resets the
            // stroke (no dabs emitted). Subsequent events use `style.dtime`.
            let dtime = if first { 10.0 } else { style.dtime as f64 };
            brush.stroke_to(&mut state, &mut surface, px, py, pressure, 0.0, 0.0, dtime);
            first = false;
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

#[inline]
fn next_unit(state: &mut u64) -> f32 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let x = (*state >> 33) as u32;
    (x as f32) * (1.0 / (1u64 << 32) as f32)
}

fn linear_rgb_to_hsv(rgb: [f32; 3]) -> (f32, f32, f32) {
    let r = linear_to_srgb(rgb[0]);
    let g = linear_to_srgb(rgb[1]);
    let b = linear_to_srgb(rgb[2]);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let v = max;
    let s = if max <= 0.0 { 0.0 } else { d / max };
    let h = if d <= 0.0 {
        0.0
    } else if (max - r).abs() < f32::EPSILON {
        (((g - b) / d).rem_euclid(6.0)) / 6.0
    } else if (max - g).abs() < f32::EPSILON {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    (h, s, v)
}

fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}
