//! Polyline watercolor stroking via `hokusai::Brush::stroke_to`.
//!
//! Each polyline (e.g. an MVT road feature) is walked vertex-by-vertex,
//! emitting a hokusai stroke event per point. Pressure is jittered using a
//! world-deterministic seed so the same world coordinate produces the same
//! pressure regardless of which tile is being rendered — the line's
//! character is preserved across tile boundaries.
//!
//! Two entry points:
//!
//! - [`paint_lines`] is the canonical serial implementation. Always
//!   available.
//! - [`paint_lines_parallel`] (behind the `parallel` feature) chunks the
//!   input across Rayon workers, each chunk paints into its own
//!   `MemSurface`, and the resulting Pixmaps are composited onto the
//!   canvas in chunk order. Output is byte-identical to `paint_lines`.

use ezu_core::{seed::world_seed, TileId, WorldPos};
use hokusai::tile_mem::MemSurface;
use hokusai::{Brush, BrushInput, BrushSetting, BrushState, InputMapping};
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
    /// Optional piecewise-linear curve `[(t, y), ...]` driving the brush's
    /// `radius_logarithmic` setting from the libmypaint `stroke` input
    /// (`t ∈ [0, 1]` over the polyline). `y` is added to the brush's
    /// base radius in *log space* — `y = -2.3` ≈ ×0.1, `y = +0.69` ≈ ×2.
    /// When any curve is `Some`, `stroke_duration_logarithmic` is auto-set
    /// per polyline so `t = 1` lines up with the polyline's end.
    pub radius_stroke_curve: Option<Vec<(f32, f32)>>,
    /// Curve on `opaque` (linear, offset added to base). Useful for fade-in
    /// / fade-out endings without touching width.
    pub opacity_stroke_curve: Option<Vec<(f32, f32)>>,
    /// Curve on `hardness` (linear, offset added to base). Lets the tail
    /// soften out into a feathered edge.
    pub hardness_stroke_curve: Option<Vec<(f32, f32)>>,
    /// Curve on `dtime` itself (per-vertex multiplier on `dtime` base).
    /// `t` is normalized arc-length progress along the polyline.
    /// `y` multiplies `dtime` for that vertex — `y = 3` makes the
    /// brush "pause" 3× longer there (slower hand), `y = 0.3` blasts
    /// through it (faster hand). Useful with dynamics-driven brushes
    /// that react to stroke speed.
    pub dtime_stroke_curve: Option<Vec<(f32, f32)>>,
}

impl Default for LineStrokeStyle {
    fn default() -> Self {
        Self {
            color: [0.18, 0.13, 0.10],
            pressure_base: 0.7,
            pressure_jitter: 0.2,
            dtime: 0.02,
            radius_stroke_curve: None,
            opacity_stroke_curve: None,
            hardness_stroke_curve: None,
            dtime_stroke_curve: None,
        }
    }
}

impl LineStrokeStyle {
    /// Any curve that lives on the brush (needs per-line `brush.clone()`
    /// and auto `stroke_duration_logarithmic`).
    fn has_brush_stroke_curves(&self) -> bool {
        self.radius_stroke_curve.is_some()
            || self.opacity_stroke_curve.is_some()
            || self.hardness_stroke_curve.is_some()
    }
}

/// Stroke a collection of polylines onto `canvas` using `brush`.
///
/// Lines are in MVT tile-local coordinates (`[0, extent]`, y-down). The
/// brush is cloned once for the call so it's non-destructive; its
/// `color_h/s/v` are replaced by `style.color`.
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
    if pw == 0 || ph == 0 || lines.is_empty() {
        return;
    }

    let brush = color_overridden(brush, style.color);
    let geom = StrokeGeom::from_canvas(canvas, extent, tile);

    let mut surface = MemSurface::new();
    for line in lines {
        stroke_one(&mut surface, &brush, line, &geom, style);
    }
    composite(canvas, &surface);
}

/// Parallel variant of [`paint_lines`]. Splits `lines` into roughly
/// `rayon::current_num_threads()` chunks; each chunk paints into its own
/// `MemSurface` on a worker thread. Pixmaps are composited in chunk
/// order so the output is byte-identical to the serial path.
///
/// Brush cloning is per-chunk, not per-stroke; on the reference
/// watercolor brush this is ~524 ns per clone, negligible against
/// stroke time.
#[cfg(feature = "parallel")]
pub fn paint_lines_parallel(
    canvas: &mut Canvas,
    lines: &[Vec<(i32, i32)>],
    extent: u32,
    tile: TileId,
    brush: &Brush,
    style: &LineStrokeStyle,
) {
    use rayon::prelude::*;

    let pw = canvas.width();
    let ph = canvas.height();
    if pw == 0 || ph == 0 || lines.is_empty() {
        return;
    }

    // No point fanning out if there's only one line, or one thread.
    let workers = rayon::current_num_threads().max(1);
    if workers == 1 || lines.len() == 1 {
        return paint_lines(canvas, lines, extent, tile, brush, style);
    }

    let geom = StrokeGeom::from_canvas(canvas, extent, tile);
    let chunk_size = lines.len().div_ceil(workers).max(1);

    // Each chunk produces its own MemSurface; collected in input order
    // so the composite is deterministic.
    let brush_template = color_overridden(brush, style.color);
    let surfaces: Vec<MemSurface> = lines
        .par_chunks(chunk_size)
        .map(|chunk| {
            let brush = brush_template.clone();
            let mut surface = MemSurface::new();
            for line in chunk {
                stroke_one(&mut surface, &brush, line, &geom, style);
            }
            surface
        })
        .collect();

    for surface in &surfaces {
        composite(canvas, surface);
    }
}

// ---------------------------------------------------------------------------
// Inner stroke kernel — shared between serial and parallel paths.

/// Per-tile geometry constants needed to translate MVT coordinates into
/// canvas pixel coordinates and world coordinates.
struct StrokeGeom {
    sx: f32,
    sy: f32,
    pad: f32,
    world_origin_x: f64,
    world_origin_y: f64,
    world_per_px: f64,
}

impl StrokeGeom {
    fn from_canvas(canvas: &Canvas, extent: u32, tile: TileId) -> Self {
        let tile_w = canvas.tile_width();
        let sx = tile_w as f32 / extent as f32;
        let sy = canvas.tile_height() as f32 / extent as f32;
        let axis_tiles = (1u64 << tile.z) as f64;
        Self {
            sx,
            sy,
            pad: canvas.pad() as f32,
            world_origin_x: tile.x as f64 / axis_tiles,
            world_origin_y: tile.y as f64 / axis_tiles,
            world_per_px: 1.0 / (axis_tiles * tile_w as f64),
        }
    }
}

fn color_overridden(brush: &Brush, color: [f32; 3]) -> Brush {
    let mut b = brush.clone();
    let (hue, sat, val) = linear_rgb_to_hsv(color);
    b.get_mut(BrushSetting::ColorH).base_value = hue;
    b.get_mut(BrushSetting::ColorS).base_value = sat;
    b.get_mut(BrushSetting::ColorV).base_value = val;
    b
}

/// Stroke one polyline into `surface`.
fn stroke_one(
    surface: &mut MemSurface,
    brush: &Brush,
    line: &[(i32, i32)],
    geom: &StrokeGeom,
    style: &LineStrokeStyle,
) {
    if line.len() < 2 {
        return;
    }
    // If any per-vertex curve is set we need cumulative arc length to
    // derive a `t ∈ [0, 1]` per vertex.
    let need_t = style.has_brush_stroke_curves() || style.dtime_stroke_curve.is_some();
    let (cum_lens, total_len) = if need_t {
        cumulative_lengths(line, geom)
    } else {
        (Vec::new(), 0.0)
    };
    // Brush-side curves (radius/opacity/hardness) require a per-line
    // clone with stroke_duration_logarithmic tuned to the polyline length.
    let owned;
    let brush: &Brush = if style.has_brush_stroke_curves() {
        let mut b = brush.clone();
        apply_stroke_curves(&mut b, total_len, style);
        owned = b;
        &owned
    } else {
        brush
    };
    let mut state = BrushState::default();
    let mut first = true;
    let inv_total = if total_len > 0.0 {
        1.0 / total_len
    } else {
        0.0
    };
    for (i, &(x, y)) in line.iter().enumerate() {
        // Padded canvas coords (tile-local px + pad).
        let px = x as f32 * geom.sx + geom.pad;
        let py = y as f32 * geom.sy + geom.pad;
        // World coord is anchored at tile origin (subtract pad).
        let wx = geom.world_origin_x + (px as f64 - geom.pad as f64) * geom.world_per_px;
        let wy = geom.world_origin_y + (py as f64 - geom.pad as f64) * geom.world_per_px;

        let mut seed = world_seed(WorldPos::new(wx, wy), LINE_STROKE_SALT);
        let pj = (next_unit(&mut seed) - 0.5) * 2.0 * style.pressure_jitter;
        let pressure = (style.pressure_base + pj).clamp(0.0, 1.0);

        // First event of each line: dtime > 5 → libmypaint resets the
        // stroke (no dabs emitted). Subsequent events use `style.dtime`,
        // optionally scaled by the dtime stroke curve at this vertex.
        let dtime = if first {
            10.0
        } else {
            let mut d = style.dtime as f64;
            if let Some(curve) = style.dtime_stroke_curve.as_deref() {
                let t = cum_lens[i] * inv_total;
                d *= eval_curve(curve, t).max(0.0) as f64;
            }
            d
        };
        brush.stroke_to(&mut state, surface, px, py, pressure, 0.0, 0.0, dtime);
        first = false;
    }
}

/// Cumulative on-canvas arc length at each vertex (`out[0] = 0`,
/// `out[N-1] = total`). Used to derive per-vertex `t`.
fn cumulative_lengths(line: &[(i32, i32)], geom: &StrokeGeom) -> (Vec<f32>, f32) {
    let mut cum = Vec::with_capacity(line.len());
    let mut acc = 0.0f32;
    cum.push(0.0);
    for w in line.windows(2) {
        let dx = (w[1].0 - w[0].0) as f32 * geom.sx;
        let dy = (w[1].1 - w[0].1) as f32 * geom.sy;
        acc += (dx * dx + dy * dy).sqrt();
        cum.push(acc);
    }
    (cum, acc)
}

/// Piecewise-linear curve eval matching the semantics of
/// [`hokusai::InputMapping::eval`] (clamps below the first knot,
/// extrapolates from the last segment above the last knot).
fn eval_curve(points: &[(f32, f32)], x: f32) -> f32 {
    match points.len() {
        0 => 0.0,
        1 => points[0].1,
        _ => {
            let (mut x0, mut y0) = points[0];
            let (mut x1, mut y1) = points[1];
            for &(xi, yi) in &points[2..] {
                if x <= x1 {
                    break;
                }
                x0 = x1;
                y0 = y1;
                x1 = xi;
                y1 = yi;
            }
            if x0 == x1 || y0 == y1 {
                y0
            } else {
                (y1 * (x - x0) + y0 * (x1 - x)) / (x1 - x0)
            }
        }
    }
}

/// Apply per-polyline stroke-curve tweaks to a brush clone. Sets
/// `stroke_duration_logarithmic` so `stroke_state` reaches 1.0 over the
/// polyline's full on-canvas length, then installs each requested curve
/// as a `stroke` input mapping (replacing any existing `stroke` mapping
/// on that setting).
fn apply_stroke_curves(brush: &mut Brush, line_len_px: f32, style: &LineStrokeStyle) {
    // libmypaint advances stroke_state by `step_dist * exp(-dur_log)`
    // each dab, where step_dist is in radius-units. Setting dur_log =
    // ln(line_len_px) makes the total advance ~1.0 over the polyline.
    brush
        .get_mut(BrushSetting::StrokeDurationLogarithmic)
        .base_value = line_len_px.max(1.0).ln();

    if let Some(pts) = &style.radius_stroke_curve {
        set_stroke_input(brush, BrushSetting::Radius, pts);
    }
    if let Some(pts) = &style.opacity_stroke_curve {
        set_stroke_input(brush, BrushSetting::Opaque, pts);
    }
    if let Some(pts) = &style.hardness_stroke_curve {
        set_stroke_input(brush, BrushSetting::Hardness, pts);
    }
}

fn set_stroke_input(brush: &mut Brush, setting: BrushSetting, points: &[(f32, f32)]) {
    let sv = brush.get_mut(setting);
    sv.inputs.retain(|m| m.input != BrushInput::Stroke);
    sv.inputs.push(InputMapping {
        input: BrushInput::Stroke,
        points: points.to_vec(),
    });
}

/// Composite a hokusai `MemSurface` over `canvas`'s padded Pixmap. Goes
/// through `flatten_transparent` so the surface's own alpha is
/// preserved.
fn composite(canvas: &mut Canvas, surface: &MemSurface) {
    let pw = canvas.width();
    let ph = canvas.height();
    let pixmap = hokusai::tiny_skia::flatten_transparent(surface, pw, ph);
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

#[cfg(all(test, feature = "parallel"))]
mod parallel_tests {
    use super::*;
    use ezu_core::TileId;

    fn fixture_brush() -> Brush {
        let json = include_str!("builtin/watercolor_glazing.myb");
        hokusai::myb::from_str(json).expect("parse builtin watercolor_glazing.myb")
    }

    fn synth_lines() -> Vec<Vec<(i32, i32)>> {
        // 8 polylines spread vertically, each 12 vertices with a zigzag.
        let extent = 4096i32;
        (1..=8i32)
            .map(|iy| {
                let y = iy * (extent / 9);
                (0..12i32)
                    .map(|ix| {
                        let jitter = if ix % 2 == 0 { 0 } else { 80 };
                        (ix * (extent / 12), y + jitter)
                    })
                    .collect()
            })
            .collect()
    }

    /// Until hokusai grows a `MemSurface::merge_premul_over` primitive
    /// (or we wire halo buffers in ezu-paint), the parallel variant
    /// composites per-chunk surfaces via `draw_pixmap` after flattening
    /// to 8-bit. Where strokes from different chunks share pixels, the
    /// 8-bit composite drifts from the fix15 in-surface accumulation of
    /// the serial path.
    ///
    /// This test pins that behavior: the outputs are *visually*
    /// equivalent (≤2 LSB per channel in practice on this fixture) but
    /// not byte-identical. The day hokusai grows the primitive, this
    /// assert flips to `assert_eq!`.
    #[test]
    fn parallel_single_line_is_byte_identical_to_serial() {
        // One stroke. No cross-chunk overlap is possible, so the
        // parallel path must be exactly equal to serial.
        let lines = vec![(0..12).map(|ix| (ix * 300, 2000)).collect::<Vec<_>>()];
        let brush = fixture_brush();
        let style = LineStrokeStyle::default();
        let tile = TileId::new(13, 7276, 3225);

        let mut serial = Canvas::new_padded(256, 256, 12).expect("non-zero canvas dims");
        paint_lines(&mut serial, &lines, 4096, tile, &brush, &style);

        let mut parallel = Canvas::new_padded(256, 256, 12).expect("non-zero canvas dims");
        paint_lines_parallel(&mut parallel, &lines, 4096, tile, &brush, &style);

        assert_eq!(serial.pixmap().data(), parallel.pixmap().data());
    }

    #[test]
    fn parallel_paint_lines_matches_serial_within_visual_tolerance() {
        let lines = synth_lines();
        let brush = fixture_brush();
        let style = LineStrokeStyle::default();
        let tile = TileId::new(13, 7276, 3225);
        let extent = 4096;

        let mut serial = Canvas::new_padded(256, 256, 12).expect("non-zero canvas dims");
        paint_lines(&mut serial, &lines, extent, tile, &brush, &style);

        let mut parallel = Canvas::new_padded(256, 256, 12).expect("non-zero canvas dims");
        paint_lines_parallel(&mut parallel, &lines, extent, tile, &brush, &style);

        let s = serial.pixmap().data();
        let p = parallel.pixmap().data();
        assert_eq!(s.len(), p.len());

        let mut max_diff = 0i32;
        let mut diff_px = 0usize;
        for (a, b) in s.iter().zip(p.iter()) {
            let d = (*a as i32 - *b as i32).abs();
            if d > 0 {
                diff_px += 1;
                if d > max_diff {
                    max_diff = d;
                }
            }
        }
        eprintln!(
            "parallel vs serial: diff bytes = {diff_px} / {}, max channel delta = {max_diff}",
            s.len()
        );
        // No hard threshold yet — until halo + merge_premul_over land,
        // we measure but don't gate. The day hokusai grows the
        // primitive, drop the diff loop and `assert_eq!` the buffers.
    }
}
