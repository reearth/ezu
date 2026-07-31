//! Paint MVT features onto a raster canvas.
//!
//! Three painting primitives are exposed:
//!
//! - [`paint_polygons`] — `tiny-skia` solid fill + optional outline +
//!   `libblur` gaussian blur. Fast path for large patches.
//! - [`paint_polygons_dabs`] — `hokusai` scatter-dab fill with
//!   world-deterministic jitter (seamless across tile boundaries).
//! - [`paint_lines`] — `hokusai::Brush::stroke_to` along polylines.
//!
//! These are the building blocks for the graph nodes in [`nodes`];
//! the host-side glue (PNG encoding, asset loading) lives in [`host`].
//!
//! All painting happens on a [`Canvas`] that optionally wraps a
//! **padded** buffer (`tile_size + 2 * pad`). Paint operations work in
//! the padded space; cropping happens at the host boundary.

// The node `schema()` methods build large `serde_json::json!` literals; the
// default macro recursion limit is not enough for the biggest of them.
#![recursion_limit = "256"]

pub mod brush;
/// Colour-space stop interpolation (re-exported from `ezu-core` so the
/// paint nodes and the MapLibre converter share one implementation).
pub use ezu_core::color as color_interp;
pub mod dabs;
pub mod render;
pub mod strokes;

pub use brush::BrushDefaults;
pub use dabs::{paint_polygons_dabs, DabFillStyle};
pub use hokusai::color::RgbaF32;
pub use hokusai::Brush;
#[cfg(feature = "parallel")]
pub use strokes::paint_lines_parallel;
pub use strokes::{paint_lines, LineStrokeStyle};

use ezu_features::Polygon;
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PixmapPaint,
    PremultipliedColorU8, Stroke, StrokeDash, Transform,
};

/// A raster canvas backed by a premultiplied RGBA `Pixmap`.
///
/// The canvas optionally has a padding ring around the tile area; all paint
/// operations work in the padded coordinate space, and [`encode_png`] crops
/// back down to the actual tile.
pub mod host;
pub mod nodes;

pub struct Canvas {
    pixmap: Pixmap,
    tile_w: u32,
    tile_h: u32,
    pad: u32,
}

impl Canvas {
    /// Convenience: padded canvas with `pad = 0`.
    /// Returns `None` if `tile_w == 0` or `tile_h == 0`, or if the
    /// pixel buffer would overflow allocation.
    pub fn new(tile_w: u32, tile_h: u32) -> Option<Self> {
        Self::new_padded(tile_w, tile_h, 0)
    }

    /// Create a canvas whose internal buffer is `tile_w + 2*pad` × `tile_h + 2*pad`.
    ///
    /// Returns `None` if the resulting padded dimensions are zero or
    /// would overflow allocation.
    pub fn new_padded(tile_w: u32, tile_h: u32, pad: u32) -> Option<Self> {
        let pw = tile_w.checked_add(2u32.checked_mul(pad)?)?;
        let ph = tile_h.checked_add(2u32.checked_mul(pad)?)?;
        let pixmap = Pixmap::new(pw, ph)?;
        Some(Self {
            pixmap,
            tile_w,
            tile_h,
            pad,
        })
    }

    /// Fill the entire (padded) canvas with a solid color, e.g. paper background.
    pub fn fill(&mut self, color: Color) {
        self.pixmap.fill(color);
    }

    pub fn pixmap(&self) -> &Pixmap {
        &self.pixmap
    }

    pub fn pixmap_mut(&mut self) -> &mut Pixmap {
        &mut self.pixmap
    }

    /// Width of the internal (padded) buffer. Use this when sizing layer
    /// pixmaps, masks, or scatter grids.
    pub fn width(&self) -> u32 {
        self.tile_w + 2 * self.pad
    }

    /// Height of the internal (padded) buffer.
    pub fn height(&self) -> u32 {
        self.tile_h + 2 * self.pad
    }

    pub fn tile_width(&self) -> u32 {
        self.tile_w
    }

    pub fn tile_height(&self) -> u32 {
        self.tile_h
    }

    pub fn pad(&self) -> u32 {
        self.pad
    }

    /// Consume the canvas and return its underlying `Pixmap`. Callers
    /// can then call `Pixmap::take` to recover the raw `Vec<u8>` without
    /// copying — paint nodes use this to hand a freshly-painted buffer
    /// to the graph layer without an intermediate `to_vec`.
    pub fn into_pixmap(self) -> Pixmap {
        self.pixmap
    }
}

/// Style for a watercolor polygon layer.
#[derive(Debug, Clone)]
pub struct WatercolorStyle {
    pub fill: Color,
    /// Optional darker outline color giving the "wet edge" feel.
    pub edge: Option<Color>,
    pub edge_width: f32,
    /// Gaussian blur sigma applied to the layer before compositing.
    pub blur_sigma: f32,
}

impl Default for WatercolorStyle {
    fn default() -> Self {
        Self {
            fill: Color::from_rgba8(150, 180, 210, 180),
            edge: Some(Color::from_rgba8(80, 110, 150, 220)),
            edge_width: 1.5,
            blur_sigma: 1.2,
        }
    }
}

/// Paint a collection of MVT polygons onto a fresh transparent layer, blur it,
/// and composite it over `canvas` (source-over).
///
/// Coordinates are MVT tile-local (`[0, extent]`, y-down). The polygons are
/// scaled to tile size and offset by the canvas's padding.
pub fn paint_polygons(
    canvas: &mut Canvas,
    polygons: &[Polygon],
    extent: u32,
    style: &WatercolorStyle,
) {
    let w = canvas.width();
    let h = canvas.height();

    let sx = canvas.tile_w as f32 / extent as f32;
    let sy = canvas.tile_h as f32 / extent as f32;
    let ox = canvas.pad as f32;
    let oy = canvas.pad as f32;

    let mut fill_paint = Paint::default();
    fill_paint.set_color(style.fill);
    fill_paint.anti_alias = true;

    let mut edge_paint = Paint::default();
    if let Some(edge) = style.edge {
        edge_paint.set_color(edge);
        edge_paint.anti_alias = true;
    }

    let draw = |target: &mut Pixmap| {
        for poly in polygons {
            let Some(path) = build_polygon_path(poly, sx, sy, ox, oy) else {
                continue;
            };
            target.fill_path(
                &path,
                &fill_paint,
                FillRule::EvenOdd,
                Transform::identity(),
                None,
            );
            if style.edge.is_some() {
                let stroke = Stroke {
                    width: style.edge_width,
                    ..Stroke::default()
                };
                target.stroke_path(&path, &edge_paint, &stroke, Transform::identity(), None);
            }
        }
    };

    if style.blur_sigma > 0.0 {
        // Blur needs an isolated layer so it only softens this call's
        // polygons, not what's already on the canvas.
        let mut layer = Pixmap::new(w, h).expect("non-zero layer");
        draw(&mut layer);
        blur_pixmap(&mut layer, style.blur_sigma);
        canvas.pixmap.draw_pixmap(
            0,
            0,
            layer.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    } else {
        draw(&mut canvas.pixmap);
    }
}

/// Style for a crisp vector stroke (contrast with `paint_lines`, which is a
/// painterly hokusai brush).
#[derive(Debug, Clone)]
pub struct StrokeStyle {
    pub color: Color,
    pub width: f32,
    pub cap: LineCap,
    pub join: LineJoin,
    /// On/off dash lengths in pixels (empty / `None` = solid).
    pub dash: Option<Vec<f32>>,
    /// MapLibre `line-gap-width` in pixels. `0` (the plain case) strokes the
    /// centreline at `width`. A positive gap turns the stroke into a casing:
    /// two parallel strokes of `width` each, their inner edges `gap` apart,
    /// i.e. an annulus of outer width `gap + 2 * width` around a `gap`-wide
    /// hole.
    pub gap: f32,
}

/// Stroke MVT polylines with a crisp, constant-width `tiny-skia` line onto a
/// fresh layer, then composite over `canvas`. Coordinates are MVT tile-local
/// (`[0, extent]`, y-down), scaled to tile size and offset by the pad.
pub fn paint_strokes(
    canvas: &mut Canvas,
    lines: &[Vec<(i32, i32)>],
    extent: u32,
    style: &StrokeStyle,
) {
    if lines.is_empty() || style.width <= 0.0 {
        return;
    }
    let sx = canvas.tile_w as f32 / extent as f32;
    let sy = canvas.tile_h as f32 / extent as f32;
    let ox = canvas.pad as f32;
    let oy = canvas.pad as f32;

    let paths: Vec<tiny_skia::Path> = lines
        .iter()
        .filter(|line| line.len() >= 2)
        .filter_map(|line| {
            let mut pb = PathBuilder::new();
            pb.move_to(line[0].0 as f32 * sx + ox, line[0].1 as f32 * sy + oy);
            for &(x, y) in &line[1..] {
                pb.line_to(x as f32 * sx + ox, y as f32 * sy + oy);
            }
            pb.finish()
        })
        .collect();
    if paths.is_empty() {
        return;
    }

    let mut paint = Paint::default();
    paint.set_color(style.color);
    paint.anti_alias = true;

    let gap = style.gap.max(0.0);
    let mut stroke = Stroke {
        // With a gap the drawn band spans `gap/2 ..= gap/2 + width` from the
        // centreline, so the outer footprint is `gap + 2 * width`.
        width: if gap > 0.0 {
            gap + 2.0 * style.width
        } else {
            style.width
        },
        line_cap: style.cap,
        line_join: style.join,
        ..Stroke::default()
    };
    if let Some(pattern) = &style.dash {
        // tiny-skia needs an even, non-empty pattern with positive total.
        if pattern.len() >= 2 && pattern.iter().sum::<f32>() > 0.0 {
            let mut p = pattern.clone();
            if p.len() % 2 == 1 {
                p.extend_from_within(..); // repeat to make it even
            }
            stroke.dash = StrokeDash::new(p, 0.0);
        }
    }

    if gap <= 0.0 {
        for path in &paths {
            canvas
                .pixmap
                .stroke_path(path, &paint, &stroke, Transform::identity(), None);
        }
        return;
    }

    // Casing: paint the full footprint onto an isolated layer, then knock the
    // `gap`-wide corridor back out of it, so the two flanks share the outer
    // stroke's joins, caps and dash phase exactly as MapLibre's line shader
    // does (it renders one extruded ribbon and discards fragments closer to
    // the centreline than `gap/2`). The knockout is solid even when the
    // casing is dashed: the corridor is empty between dashes anyway.
    let Some(mut layer) = Pixmap::new(canvas.pixmap.width(), canvas.pixmap.height()) else {
        return;
    };
    for path in &paths {
        layer.stroke_path(path, &paint, &stroke, Transform::identity(), None);
    }
    let mut erase = Paint {
        blend_mode: tiny_skia::BlendMode::DestinationOut,
        anti_alias: true,
        ..Paint::default()
    };
    erase.set_color(Color::BLACK);
    let hole = Stroke {
        width: gap,
        line_cap: style.cap,
        line_join: style.join,
        ..Stroke::default()
    };
    for path in &paths {
        layer.stroke_path(path, &erase, &hole, Transform::identity(), None);
    }
    canvas.pixmap.draw_pixmap(
        0,
        0,
        layer.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
}

pub(crate) fn build_polygon_path(
    poly: &Polygon,
    sx: f32,
    sy: f32,
    ox: f32,
    oy: f32,
) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    push_ring(&mut pb, &poly.exterior, sx, sy, ox, oy)?;
    for hole in &poly.holes {
        push_ring(&mut pb, hole, sx, sy, ox, oy)?;
    }
    pb.finish()
}

fn push_ring(
    pb: &mut PathBuilder,
    ring: &[(i32, i32)],
    sx: f32,
    sy: f32,
    ox: f32,
    oy: f32,
) -> Option<()> {
    if ring.len() < 3 {
        return None;
    }
    let (x0, y0) = ring[0];
    pb.move_to(x0 as f32 * sx + ox, y0 as f32 * sy + oy);
    for &(x, y) in &ring[1..] {
        pb.line_to(x as f32 * sx + ox, y as f32 * sy + oy);
    }
    pb.close();
    Some(())
}

/// In-place gaussian blur on a tiny-skia `Pixmap` using `libblur`.
fn blur_pixmap(pixmap: &mut Pixmap, sigma: f32) {
    let w = pixmap.width() as usize;
    let h = pixmap.height() as usize;
    let mut rgba: Vec<u8> = Vec::with_capacity(w * h * 4);
    for px in pixmap.pixels() {
        let p = px.demultiply();
        rgba.extend_from_slice(&[p.red(), p.green(), p.blue(), p.alpha()]);
    }

    let src_buf = rgba.clone();
    let src = libblur::BlurImage::borrow(
        &src_buf,
        w as u32,
        h as u32,
        libblur::FastBlurChannels::Channels4,
    );
    let mut dst = libblur::BlurImageMut::borrow(
        &mut rgba,
        w as u32,
        h as u32,
        libblur::FastBlurChannels::Channels4,
    );
    if libblur::gaussian_blur(
        &src,
        &mut dst,
        libblur::GaussianBlurParams::new_from_sigma(sigma as f64),
        libblur::EdgeMode2D::new(libblur::EdgeMode::Clamp),
        libblur::ThreadingPolicy::Single,
        libblur::ConvolutionMode::Exact,
    )
    .is_err()
    {
        return;
    }

    let out = pixmap.pixels_mut();
    for (i, dst) in out.iter_mut().enumerate() {
        let r = rgba[i * 4];
        let g = rgba[i * 4 + 1];
        let b = rgba[i * 4 + 2];
        let a = rgba[i * 4 + 3];
        *dst = PremultipliedColorU8::from_rgba(mul(r, a), mul(g, a), mul(b, a), a).unwrap_or_else(
            || {
                // Fully-transparent black is always a valid premul color;
                // this fallback only fires if `from_rgba` ever rejects
                // its input (it doesn't today).
                PremultipliedColorU8::from_rgba(0, 0, 0, 0)
                    .expect("transparent black is a valid premul color")
            },
        );
    }
}

#[inline]
fn mul(c: u8, a: u8) -> u8 {
    ((c as u16 * a as u16 + 127) / 255) as u8
}

#[derive(Debug, thiserror::Error)]
pub enum PaintError {
    #[error("png encode failed")]
    PngEncode,
    #[error("webp encode failed: {0}")]
    WebpEncode(String),
}
