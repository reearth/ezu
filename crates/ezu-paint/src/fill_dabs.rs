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
    if canvas.width() == 0 || canvas.height() == 0 || polygons.is_empty() {
        return;
    }
    let mask: Vec<u32> = rasterize_mask(polygons, extent, canvas)
        .into_iter()
        .map(u32::from)
        .collect();
    scatter(canvas, &mask, &[(style.color, style.opacity)], tile, style);
}

/// One group of polygons for [`paint_polygon_groups_dabs`], with the color
/// and opacity its dabs carry.
pub(crate) struct DabGroup<'a> {
    pub(crate) polygons: &'a [Polygon],
    pub(crate) color: RgbaF32,
    pub(crate) opacity: f32,
}

/// [`paint_polygons_dabs`] for polygons that do not all share one color and
/// opacity. The lattice is scattered once over the union of the groups, and
/// each dab takes the paint of the group whose polygon covers its centre —
/// the last such group, as the last-drawn polygon is the one on top. So the
/// dabs land, and jitter, exactly where one call over all the polygons puts
/// them, and only their paint differs: every other field of `style` is
/// shared, and its own `color` and `opacity` are not used.
pub(crate) fn paint_polygon_groups_dabs(
    canvas: &mut Canvas,
    groups: &[DabGroup<'_>],
    extent: u32,
    tile: TileId,
    style: &DabFillStyle,
) {
    let pw = canvas.width();
    let ph = canvas.height();
    if pw == 0 || ph == 0 || groups.iter().all(|g| g.polygons.is_empty()) {
        return;
    }
    let mut mask = vec![0u32; (pw * ph) as usize];
    let mut scratch = tiny_skia::Pixmap::new(pw, ph).expect("non-zero mask size");
    for (i, group) in groups.iter().enumerate() {
        let Some(rect) = fill_mask_pixmap(&mut scratch, group.polygons, extent, canvas) else {
            continue;
        };
        // Only the group's own bounds were drawn, so only they are read back
        // and cleared for the next group.
        let index = i as u32 + 1;
        for y in rect.top() as u32..rect.bottom() as u32 {
            for x in rect.left() as u32..rect.right() as u32 {
                let at = (y * pw + x) as usize;
                if scratch.pixels()[at].alpha() > 0 {
                    mask[at] = index;
                }
                scratch.pixels_mut()[at] = tiny_skia::PremultipliedColorU8::TRANSPARENT;
            }
        }
    }
    let paints: Vec<(RgbaF32, f32)> = groups.iter().map(|g| (g.color, g.opacity)).collect();
    scatter(canvas, &mask, &paints, tile, style);
}

/// Scatter the world-anchored dab lattice over `canvas`, drawing a dab
/// wherever `mask` is non-zero at its centre, in the color and opacity
/// `paints[mask - 1]` gives it; `style` supplies everything else.
fn scatter(
    canvas: &mut Canvas,
    mask: &[u32],
    paints: &[(RgbaF32, f32)],
    tile: TileId,
    style: &DabFillStyle,
) {
    let pw = canvas.width();
    let ph = canvas.height();
    let tile_w = canvas.tile_width();
    let pad = canvas.pad();
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
            let index = mask[(iy * pw + ix) as usize];
            if index == 0 {
                continue;
            }
            let (color, opacity) = paints[index as usize - 1];

            let size_mult = 1.0 + (next_unit(&mut state) - 0.5) * 2.0 * style.size_jitter;
            let opacity_mult = 1.0 + (next_unit(&mut state) - 0.5) * 2.0 * style.opacity_jitter;
            let value_jit = (next_unit(&mut state) - 0.5) * 2.0 * style.value_jitter;

            let dab = Dab {
                x: dab_x,
                y: dab_y,
                radius: (style.radius_px * size_mult).max(0.5),
                color: RgbaF32 {
                    r: (color.r + value_jit).clamp(0.0, 1.0),
                    g: (color.g + value_jit).clamp(0.0, 1.0),
                    b: (color.b + value_jit).clamp(0.0, 1.0),
                    a: 1.0,
                },
                opaque: (opacity * opacity_mult).clamp(0.0, 1.0),
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
    fill_mask_pixmap(&mut pixmap, polygons, extent, canvas);
    pixmap.pixels().iter().map(|p| p.alpha() > 0).collect()
}

/// Fill `polygons` into `pixmap` (canvas-sized) the way the mask is drawn:
/// opaque white, without anti-aliasing, even-odd. Returns the pixel rect the
/// fills can have touched, clamped to the pixmap, or `None` when there is
/// none.
fn fill_mask_pixmap(
    pixmap: &mut tiny_skia::Pixmap,
    polygons: &[Polygon],
    extent: u32,
    canvas: &Canvas,
) -> Option<tiny_skia::IntRect> {
    let pw = canvas.width();
    let ph = canvas.height();
    let mut paint = Paint::default();
    paint.set_color(Color::WHITE);
    paint.anti_alias = false;

    let sx = canvas.tile_width() as f32 / extent as f32;
    let sy = canvas.tile_height() as f32 / extent as f32;
    let ox = canvas.pad() as f32;
    let oy = canvas.pad() as f32;
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for poly in polygons {
        if let Some(path) = build_polygon_path(poly, sx, sy, ox, oy) {
            let b = path.bounds();
            x0 = x0.min(b.left());
            y0 = y0.min(b.top());
            x1 = x1.max(b.right());
            y1 = y1.max(b.bottom());
            pixmap.fill_path(
                &path,
                &paint,
                FillRule::EvenOdd,
                Transform::identity(),
                None,
            );
        }
    }
    if x0 > x1 || y0 > y1 {
        return None;
    }
    // One pixel of slack either side covers the rounding of the fill's
    // pixel coverage at the bounds' edges.
    let left = (x0.floor() as i64 - 1).clamp(0, pw as i64) as u32;
    let top = (y0.floor() as i64 - 1).clamp(0, ph as i64) as u32;
    let right = (x1.ceil() as i64 + 1).clamp(0, pw as i64) as u32;
    let bottom = (y1.ceil() as i64 + 1).clamp(0, ph as i64) as u32;
    tiny_skia::IntRect::from_ltrb(left as i32, top as i32, right as i32, bottom as i32)
}
