//! Rasterize a laid-out [`TextBlock`] with tiny-skia.
//!
//! Outline glyphs fill/stroke their vector paths; SDF glyphs evaluate
//! the maplibre-gl-js `symbol_sdf` fragment math per pixel (see
//! [`super::sdf`] for the encoding constants and compat quirks).
//!
//! An outline glyph can also be drawn through the SDF path: passing an
//! [`OutlineSdfCache`] to [`draw`] / [`draw_line`] routes outline glyphs
//! through the same field-sampling code as glyph-PBF text, matching
//! maplibre-gl-js (which renders every glyph from an SDF). Its halo then
//! comes free from a distance threshold rather than a `2 × halo` stroke.
//! Passing `None` keeps the vector fill / stroke path.

use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PixmapMut, Point, PremultipliedColorU8, Stroke,
    Transform,
};

use super::font::FaceEntry;
use super::layout::{PlacedGlyph, TextBlock};
use super::outline_sdf::OutlineSdfCache;
use super::sdf::{SdfGlyph, SDF_BORDER, SDF_EDGE, SDF_EM_PX, SDF_RADIUS_PX};

/// How to paint a block: font size in device px plus fill / halo
/// colors. Colors are straight (non-premultiplied) sRGB RGBA in
/// `[0, 1]` — the same convention as a parsed `#rrggbb[aa]` literal.
#[derive(Debug, Clone, Copy)]
pub struct TextPaint {
    pub size_px: f32,
    pub color: [f32; 4],
    pub halo_color: [f32; 4],
    /// Halo radius in px around each glyph edge. `0` disables the halo.
    /// SDF glyphs saturate at `¼ em` — the field encodes no more
    /// distance (MapLibre's documented `text-halo-width` maximum).
    pub halo_width_px: f32,
    /// Halo edge softening in px (MapLibre `text-halo-blur`). Only the
    /// SDF backend renders it; outline halos stay crisp.
    pub halo_blur_px: f32,
}

/// Per-`format`-section fill override, indexed by [`PlacedGlyph::section`].
/// A `None` color (or a section past the end of the table) falls back to
/// [`TextPaint::color`]. Halo color/width stay block-level — MapLibre `format`
/// has no per-section halo. Callers pass an empty slice for unstyled text.
#[derive(Debug, Clone, Copy, Default)]
pub struct SectionPaint {
    /// Straight sRGB RGBA in `[0, 1]`, with the group opacity already
    /// multiplied into alpha by the caller.
    pub color: Option<[f32; 4]>,
}

/// The fill color for a glyph: its section's override, else the block color.
fn glyph_fill(paint: &TextPaint, sections: &[SectionPaint], section: u16) -> [f32; 4] {
    sections
        .get(section as usize)
        .and_then(|s| s.color)
        .unwrap_or(paint.color)
}

/// The shader's `EDGE_GAMMA` anti-aliasing constant (at device pixel
/// ratio 1 — ezu tiles are 1:1).
const EDGE_GAMMA: f32 = 0.105;

/// The `(buff, gamma)` a halo field pass uses: the edge pulled out by
/// `halo_width_px` (in SDF px, saturating where the field runs out of
/// encoded distance), with `halo_blur_px` widening the AA ramp. Shared by
/// the glyph-PBF and outline-via-SDF halo passes.
fn sdf_halo_params(halo_width_px: f32, halo_blur_px: f32, font_scale: f32) -> (f32, f32) {
    let width_sdf_px = (halo_width_px / font_scale).min(SDF_RADIUS_PX * SDF_EDGE);
    let buff = SDF_EDGE - width_sdf_px / SDF_RADIUS_PX;
    let gamma = (halo_blur_px * 1.19 / SDF_RADIUS_PX + EDGE_GAMMA) / font_scale;
    (buff, gamma)
}

/// Draw `block` onto `pixmap` with its anchor point at `origin`
/// (device px), anti-aliased.
///
/// Two passes — first *every* glyph's halo (outline: path stroked at
/// `2 × halo-width` with round joins/caps; SDF: the halo-threshold
/// field pass), then *every* glyph's fill — never interleaved per
/// glyph, so one glyph's halo cannot overpaint a neighbour's fill (the
/// MapLibre halo rule).
///
/// `outline` optionally routes outline glyphs through the SDF path (see
/// the module docs); `None` keeps the vector fill / stroke.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    block: &TextBlock,
    fonts: &[FaceEntry<'_>],
    pixmap: &mut PixmapMut<'_>,
    origin: (f32, f32),
    paint: &TextPaint,
    sections: &[SectionPaint],
    outline: Option<&OutlineSdfCache>,
) {
    if block.is_empty() || paint.size_px <= 0.0 {
        return;
    }
    // Glyph outlines are in font units (y-up); flip and scale to px. `g.x`/
    // `g.y` are em block coordinates (advances already carry the section
    // scale), so positions use the base size; only the glyph's ink `scale`
    // carries the per-section `font-scale`.
    let transform_of = |g: &PlacedGlyph, scale: f32| {
        Transform::from_translate(
            origin.0 + g.x * paint.size_px,
            origin.1 + g.y * paint.size_px,
        )
        .pre_scale(scale, -scale)
    };

    if paint.halo_width_px > 0.0 {
        let mut halo = Paint::default();
        halo.set_color(color_of(paint.halo_color));
        halo.anti_alias = true;
        for g in &block.glyphs {
            // SDF glyphs are rasterized at the 24 px em, per-glyph scaled.
            let font_scale = paint.size_px * g.scale / SDF_EM_PX;
            match &fonts[g.font] {
                FaceEntry::Outline { font, face } => match outline
                    .and_then(|c| c.get(font, face, g.glyph_id))
                {
                    // Outline glyph through the SDF halo threshold (free
                    // halo), same field math as glyph-PBF text.
                    Some(glyph) => {
                        let (buff, gamma) =
                            sdf_halo_params(paint.halo_width_px, paint.halo_blur_px, font_scale);
                        draw_sdf_glyph(
                            pixmap,
                            &glyph,
                            sdf_pen(origin, g, paint.size_px),
                            font_scale,
                            paint.halo_color,
                            buff,
                            gamma,
                        );
                    }
                    // Vector halo: the path stroked at `2 × halo-width`.
                    None => {
                        let Some(path) = font.glyph_path(face, g.glyph_id) else {
                            continue;
                        };
                        let scale = paint.size_px * g.scale / font.units_per_em();
                        // The stroke runs in path (font-unit) space; the
                        // transform scales it back to `2 × halo-width` px (a
                        // fixed px width, independent of `font-scale`).
                        let stroke = Stroke {
                            width: 2.0 * paint.halo_width_px / scale,
                            line_cap: LineCap::Round,
                            line_join: LineJoin::Round,
                            ..Stroke::default()
                        };
                        pixmap.stroke_path(&path, &halo, &stroke, transform_of(g, scale), None);
                    }
                },
                FaceEntry::Sdf(stack) => {
                    let Some(glyph) = sdf_glyph_of(stack, g.glyph_id) else {
                        continue;
                    };
                    // The shader's halo threshold: the edge pulled out by
                    // `halo-width` (in SDF px), saturating where the field
                    // runs out of encoded distance (6 px outside the edge);
                    // blur widens the AA ramp.
                    let (buff, gamma) =
                        sdf_halo_params(paint.halo_width_px, paint.halo_blur_px, font_scale);
                    draw_sdf_glyph(
                        pixmap,
                        &glyph,
                        sdf_pen(origin, g, paint.size_px),
                        font_scale,
                        paint.halo_color,
                        buff,
                        gamma,
                    );
                }
            }
        }
    }

    for g in &block.glyphs {
        let font_scale = paint.size_px * g.scale / SDF_EM_PX;
        let color = glyph_fill(paint, sections, g.section);
        match &fonts[g.font] {
            FaceEntry::Outline { font, face } => {
                match outline.and_then(|c| c.get(font, face, g.glyph_id)) {
                    // Outline glyph through the SDF fill threshold.
                    Some(glyph) => draw_sdf_glyph(
                        pixmap,
                        &glyph,
                        sdf_pen(origin, g, paint.size_px),
                        font_scale,
                        color,
                        SDF_EDGE,
                        EDGE_GAMMA / font_scale,
                    ),
                    // Vector fill of the glyph path.
                    None => {
                        let Some(path) = font.glyph_path(face, g.glyph_id) else {
                            continue;
                        };
                        let scale = paint.size_px * g.scale / font.units_per_em();
                        let mut fill = Paint::default();
                        fill.set_color(color_of(color));
                        fill.anti_alias = true;
                        pixmap.fill_path(
                            &path,
                            &fill,
                            FillRule::Winding,
                            transform_of(g, scale),
                            None,
                        );
                    }
                }
            }
            FaceEntry::Sdf(stack) => {
                let Some(glyph) = sdf_glyph_of(stack, g.glyph_id) else {
                    continue;
                };
                draw_sdf_glyph(
                    pixmap,
                    &glyph,
                    sdf_pen(origin, g, paint.size_px),
                    font_scale,
                    color,
                    SDF_EDGE,
                    EDGE_GAMMA / font_scale,
                );
            }
        }
    }
}

/// Resolve a placed SDF glyph back to its bitmap. The range was loaded
/// during shaping, so this is a pure map lookup.
fn sdf_glyph_of(
    stack: &super::sdf::SdfFontStack,
    codepoint: u16,
) -> Option<std::sync::Arc<SdfGlyph>> {
    char::from_u32(u32::from(codepoint)).and_then(|c| stack.glyph(c))
}

/// A placed glyph's pen position in device px.
fn sdf_pen(origin: (f32, f32), g: &super::layout::PlacedGlyph, size_px: f32) -> (f32, f32) {
    (origin.0 + g.x * size_px, origin.1 + g.y * size_px)
}

/// One SDF glyph, one field pass — the maplibre-gl-js `symbol_sdf`
/// fragment math on the CPU:
///
/// ```text
/// alpha = smoothstep(buff − gamma, buff + gamma, dist)
/// ```
///
/// where `dist` is the bilinearly-sampled field value (`0..=1`), `buff`
/// the threshold (0.75 at the glyph edge; lower for halos) and `gamma`
/// the AA ramp half-width. `color` is straight sRGB; output blends
/// premultiplied source-over.
fn draw_sdf_glyph(
    pixmap: &mut PixmapMut<'_>,
    glyph: &SdfGlyph,
    pen: (f32, f32),
    font_scale: f32,
    color: [f32; 4],
    buff: f32,
    gamma: f32,
) {
    if glyph.bitmap.is_empty() || color[3] <= 0.0 {
        return;
    }
    let bw = (glyph.width + 2 * SDF_BORDER) as i32;
    let bh = (glyph.height + 2 * SDF_BORDER) as i32;
    // Bitmap top-left in device px: the pen plus the ink bearings minus
    // the baked border (quads.ts corner math, atlas padding cancelled).
    let x0 = pen.0 + (glyph.left - SDF_BORDER as i32) as f32 * font_scale;
    let y0 = pen.1 + (-glyph.top - SDF_BORDER as i32) as f32 * font_scale;
    let w = bw as f32 * font_scale;
    let h = bh as f32 * font_scale;

    let width = pixmap.width() as i32;
    let height = pixmap.height() as i32;
    let px0 = (x0.floor() as i32).max(0);
    let py0 = (y0.floor() as i32).max(0);
    let px1 = ((x0 + w).ceil() as i32).min(width);
    let py1 = ((y0 + h).ceil() as i32).min(height);
    if px0 >= px1 || py0 >= py1 {
        return;
    }

    let (cr, cg, cb, ca) = (
        color[0].clamp(0.0, 1.0),
        color[1].clamp(0.0, 1.0),
        color[2].clamp(0.0, 1.0),
        color[3].clamp(0.0, 1.0),
    );
    let pixels = pixmap.pixels_mut();
    for py in py0..py1 {
        for px in px0..px1 {
            // Device pixel centre → bitmap sample coordinates.
            let sx = (px as f32 + 0.5 - x0) / font_scale - 0.5;
            let sy = (py as f32 + 0.5 - y0) / font_scale - 0.5;
            let dist = sample_bilinear(&glyph.bitmap, bw, bh, sx, sy);
            let alpha = smoothstep(buff - gamma, buff + gamma, dist) * ca;
            if alpha <= 0.0 {
                continue;
            }
            // Premultiplied source-over.
            let sa = alpha;
            let (sr, sg, sb) = (cr * sa, cg * sa, cb * sa);
            let ix = (py * width + px) as usize;
            let d = pixels[ix];
            let inv = 1.0 - sa;
            let out_r = sr + d.red() as f32 / 255.0 * inv;
            let out_g = sg + d.green() as f32 / 255.0 * inv;
            let out_b = sb + d.blue() as f32 / 255.0 * inv;
            let out_a = sa + d.alpha() as f32 / 255.0 * inv;
            let a8 = (out_a * 255.0 + 0.5) as u8;
            let quantize = |v: f32| ((v * 255.0 + 0.5) as u8).min(a8);
            pixels[ix] = PremultipliedColorU8::from_rgba(
                quantize(out_r),
                quantize(out_g),
                quantize(out_b),
                a8,
            )
            .expect("premultiplied components clamped to alpha");
        }
    }
}

/// Bilinearly sample the SDF at `(x, y)` (bitmap-local px), reading
/// out-of-bounds texels as 0 — the field has faded out at the border.
fn sample_bilinear(bitmap: &[u8], bw: i32, bh: i32, x: f32, y: f32) -> f32 {
    let fx = x.floor();
    let fy = y.floor();
    let tx = x - fx;
    let ty = y - fy;
    let (ix, iy) = (fx as i32, fy as i32);
    let texel = |dx: i32, dy: i32| -> f32 {
        let (x, y) = (ix + dx, iy + dy);
        if x < 0 || y < 0 || x >= bw || y >= bh {
            return 0.0;
        }
        bitmap[(y * bw + x) as usize] as f32 / 255.0
    };
    let top = texel(0, 0) * (1.0 - tx) + texel(1, 0) * tx;
    let bottom = texel(0, 1) * (1.0 - tx) + texel(1, 1) * tx;
    top * (1.0 - ty) + bottom * ty
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn color_of(c: [f32; 4]) -> Color {
    Color::from_rgba(
        c[0].clamp(0.0, 1.0),
        c[1].clamp(0.0, 1.0),
        c[2].clamp(0.0, 1.0),
        c[3].clamp(0.0, 1.0),
    )
    .expect("components clamped to [0, 1]")
}

// ---------------------------------------------------------------------------
// Line placement — each glyph is a rigid stamp placed at its horizontal
// centre along the path and rotated to the local tangent.

/// Where a line-placed glyph's horizontal centre sits (device px) and the
/// tangent angle (radians) to rotate it by. Index-aligned with the
/// block's glyphs (see [`super::line::place_glyphs`]).
#[derive(Debug, Clone, Copy)]
pub struct GlyphPlacement {
    pub x: f32,
    pub y: f32,
    pub angle: f32,
}

/// The device transform that maps one glyph's *local* frame (origin at
/// the pen, x along the reading direction, y down) so its horizontal
/// centre lands on the placement point rotated to the tangent. `perp` is
/// the extra `offset-em[1]` shift perpendicular to the line (device px).
fn line_glyph_transform(g: &PlacedGlyph, p: &GlyphPlacement, size: f32, perp: f32) -> Transform {
    Transform::from_translate(p.x, p.y)
        .pre_rotate(p.angle.to_degrees())
        // The pen sits half an advance behind the centre; the baseline is
        // `g.y` below the label's vertical centre, plus the perp offset.
        .pre_translate(-0.5 * g.advance * size, g.y * size + perp)
}

/// Draw `block` along a path: one rigid, tangent-rotated stamp per glyph.
///
/// `placements` is index-aligned with `block.glyphs`; `perp_offset_px` is
/// the label's perpendicular (`offset-em[1]`) shift in device px. Halo
/// then fill, both over every glyph — the same two-pass order as
/// [`draw`], so a glyph's halo never overpaints a neighbour's fill.
#[allow(clippy::too_many_arguments)]
pub fn draw_line(
    block: &TextBlock,
    fonts: &[FaceEntry<'_>],
    pixmap: &mut PixmapMut<'_>,
    placements: &[GlyphPlacement],
    perp_offset_px: f32,
    paint: &TextPaint,
    sections: &[SectionPaint],
    outline: Option<&OutlineSdfCache>,
) {
    if block.is_empty() || paint.size_px <= 0.0 || placements.len() != block.glyphs.len() {
        return;
    }
    let size = paint.size_px;

    if paint.halo_width_px > 0.0 {
        let mut halo = Paint::default();
        halo.set_color(color_of(paint.halo_color));
        halo.anti_alias = true;
        for (g, p) in block.glyphs.iter().zip(placements) {
            let font_scale = size * g.scale / SDF_EM_PX;
            let t = line_glyph_transform(g, p, size, perp_offset_px);
            match &fonts[g.font] {
                FaceEntry::Outline { font, face } => match outline
                    .and_then(|c| c.get(font, face, g.glyph_id))
                {
                    Some(glyph) => {
                        let (buff, gamma) =
                            sdf_halo_params(paint.halo_width_px, paint.halo_blur_px, font_scale);
                        draw_sdf_glyph_rotated(
                            pixmap,
                            &glyph,
                            t,
                            font_scale,
                            paint.halo_color,
                            buff,
                            gamma,
                        );
                    }
                    None => {
                        let Some(path) = font.glyph_path(face, g.glyph_id) else {
                            continue;
                        };
                        let scale = size * g.scale / font.units_per_em();
                        let stroke = Stroke {
                            width: 2.0 * paint.halo_width_px / scale,
                            line_cap: LineCap::Round,
                            line_join: LineJoin::Round,
                            ..Stroke::default()
                        };
                        pixmap.stroke_path(&path, &halo, &stroke, t.pre_scale(scale, -scale), None);
                    }
                },
                FaceEntry::Sdf(stack) => {
                    let Some(glyph) = sdf_glyph_of(stack, g.glyph_id) else {
                        continue;
                    };
                    let (buff, gamma) =
                        sdf_halo_params(paint.halo_width_px, paint.halo_blur_px, font_scale);
                    draw_sdf_glyph_rotated(
                        pixmap,
                        &glyph,
                        t,
                        font_scale,
                        paint.halo_color,
                        buff,
                        gamma,
                    );
                }
            }
        }
    }

    for (g, p) in block.glyphs.iter().zip(placements) {
        let font_scale = size * g.scale / SDF_EM_PX;
        let color = glyph_fill(paint, sections, g.section);
        let t = line_glyph_transform(g, p, size, perp_offset_px);
        match &fonts[g.font] {
            FaceEntry::Outline { font, face } => {
                match outline.and_then(|c| c.get(font, face, g.glyph_id)) {
                    Some(glyph) => draw_sdf_glyph_rotated(
                        pixmap,
                        &glyph,
                        t,
                        font_scale,
                        color,
                        SDF_EDGE,
                        EDGE_GAMMA / font_scale,
                    ),
                    None => {
                        let Some(path) = font.glyph_path(face, g.glyph_id) else {
                            continue;
                        };
                        let scale = size * g.scale / font.units_per_em();
                        let mut fill = Paint::default();
                        fill.set_color(color_of(color));
                        fill.anti_alias = true;
                        pixmap.fill_path(
                            &path,
                            &fill,
                            FillRule::Winding,
                            t.pre_scale(scale, -scale),
                            None,
                        );
                    }
                }
            }
            FaceEntry::Sdf(stack) => {
                let Some(glyph) = sdf_glyph_of(stack, g.glyph_id) else {
                    continue;
                };
                draw_sdf_glyph_rotated(
                    pixmap,
                    &glyph,
                    t,
                    font_scale,
                    color,
                    SDF_EDGE,
                    EDGE_GAMMA / font_scale,
                );
            }
        }
    }
}

/// [`draw_sdf_glyph`] generalized to an arbitrary affine `local_to_dev`
/// (mapping the glyph's local px frame — pen origin, y down — to device):
/// the field is sampled through the inverse transform with bilinear
/// filtering, so a rotated glyph stays smooth. Same `symbol_sdf` alpha
/// math and premultiplied source-over blend as the axis-aligned path.
#[allow(clippy::too_many_arguments)]
fn draw_sdf_glyph_rotated(
    pixmap: &mut PixmapMut<'_>,
    glyph: &SdfGlyph,
    local_to_dev: Transform,
    font_scale: f32,
    color: [f32; 4],
    buff: f32,
    gamma: f32,
) {
    if glyph.bitmap.is_empty() || color[3] <= 0.0 {
        return;
    }
    let Some(dev_to_local) = local_to_dev.invert() else {
        return;
    };
    let bw = (glyph.width + 2 * SDF_BORDER) as i32;
    let bh = (glyph.height + 2 * SDF_BORDER) as i32;
    // The glyph's bitmap rect in local px (matches the axis-aligned path's
    // top-left/extent, before the transform).
    let x0 = (glyph.left - SDF_BORDER as i32) as f32 * font_scale;
    let y0 = (-glyph.top - SDF_BORDER as i32) as f32 * font_scale;
    let w = bw as f32 * font_scale;
    let h = bh as f32 * font_scale;

    // Device bounding box of the (possibly rotated) glyph rect.
    let mut corners = [
        Point::from_xy(x0, y0),
        Point::from_xy(x0 + w, y0),
        Point::from_xy(x0, y0 + h),
        Point::from_xy(x0 + w, y0 + h),
    ];
    local_to_dev.map_points(&mut corners);
    let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
    let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
    for c in corners {
        min_x = min_x.min(c.x);
        max_x = max_x.max(c.x);
        min_y = min_y.min(c.y);
        max_y = max_y.max(c.y);
    }

    let width = pixmap.width() as i32;
    let height = pixmap.height() as i32;
    let px0 = (min_x.floor() as i32).max(0);
    let py0 = (min_y.floor() as i32).max(0);
    let px1 = ((max_x).ceil() as i32).min(width);
    let py1 = ((max_y).ceil() as i32).min(height);
    if px0 >= px1 || py0 >= py1 {
        return;
    }

    let (cr, cg, cb, ca) = (
        color[0].clamp(0.0, 1.0),
        color[1].clamp(0.0, 1.0),
        color[2].clamp(0.0, 1.0),
        color[3].clamp(0.0, 1.0),
    );
    let pixels = pixmap.pixels_mut();
    for py in py0..py1 {
        for px in px0..px1 {
            // Device pixel centre → local px → bitmap sample coordinates.
            let mut pt = Point::from_xy(px as f32 + 0.5, py as f32 + 0.5);
            dev_to_local.map_point(&mut pt);
            let sx = (pt.x - x0) / font_scale - 0.5;
            let sy = (pt.y - y0) / font_scale - 0.5;
            let dist = sample_bilinear(&glyph.bitmap, bw, bh, sx, sy);
            let alpha = smoothstep(buff - gamma, buff + gamma, dist) * ca;
            if alpha <= 0.0 {
                continue;
            }
            let sa = alpha;
            let (sr, sg, sb) = (cr * sa, cg * sa, cb * sa);
            let ix = (py * width + px) as usize;
            let d = pixels[ix];
            let inv = 1.0 - sa;
            let out_r = sr + d.red() as f32 / 255.0 * inv;
            let out_g = sg + d.green() as f32 / 255.0 * inv;
            let out_b = sb + d.blue() as f32 / 255.0 * inv;
            let out_a = sa + d.alpha() as f32 / 255.0 * inv;
            let a8 = (out_a * 255.0 + 0.5) as u8;
            let quantize = |v: f32| ((v * 255.0 + 0.5) as u8).min(a8);
            pixels[ix] = PremultipliedColorU8::from_rgba(
                quantize(out_r),
                quantize(out_g),
                quantize(out_b),
                a8,
            )
            .expect("premultiplied components clamped to alpha");
        }
    }
}
