//! Rasterize a laid-out [`TextBlock`] with tiny-skia.

use std::sync::Arc;

use tiny_skia::{Color, FillRule, LineCap, LineJoin, Paint, PixmapMut, Stroke, Transform};

use super::font::Font;
use super::layout::TextBlock;

/// How to paint a block: font size in device px plus fill / halo
/// colors. Colors are straight (non-premultiplied) sRGB RGBA in
/// `[0, 1]` — the same convention as a parsed `#rrggbb[aa]` literal.
#[derive(Debug, Clone, Copy)]
pub struct TextPaint {
    pub size_px: f32,
    pub color: [f32; 4],
    pub halo_color: [f32; 4],
    /// Halo radius in px around each glyph edge. `0` disables the halo.
    pub halo_width_px: f32,
}

/// Draw `block` onto `pixmap` with its anchor point at `origin`
/// (device px), anti-aliased.
///
/// Two passes — first *every* glyph's outline stroked at `2 ×
/// halo-width` (round joins/caps) in the halo color, then *every*
/// glyph filled — never interleaved per glyph, so one glyph's halo
/// cannot overpaint a neighbour's fill (the MapLibre halo rule).
pub fn draw(
    block: &TextBlock,
    fonts: &[Arc<Font>],
    pixmap: &mut PixmapMut<'_>,
    origin: (f32, f32),
    paint: &TextPaint,
) {
    if block.is_empty() || paint.size_px <= 0.0 {
        return;
    }
    // Glyph outlines are in font units (y-up); flip and scale to px.
    let transform_of = |g: &super::layout::PlacedGlyph, scale: f32| {
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
            let font = &fonts[g.font];
            let Some(path) = font.glyph_path(g.glyph_id) else {
                continue;
            };
            let scale = paint.size_px / font.units_per_em();
            // The stroke runs in path (font-unit) space; the transform
            // scales it back to `2 × halo-width` px.
            let stroke = Stroke {
                width: 2.0 * paint.halo_width_px / scale,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            pixmap.stroke_path(&path, &halo, &stroke, transform_of(g, scale), None);
        }
    }

    let mut fill = Paint::default();
    fill.set_color(color_of(paint.color));
    fill.anti_alias = true;
    for g in &block.glyphs {
        let font = &fonts[g.font];
        let Some(path) = font.glyph_path(g.glyph_id) else {
            continue;
        };
        let scale = paint.size_px / font.units_per_em();
        pixmap.fill_path(
            &path,
            &fill,
            FillRule::Winding,
            transform_of(g, scale),
            None,
        );
    }
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
