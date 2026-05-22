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

pub mod brush;
pub mod dabs;
pub mod render;
pub mod strokes;

pub use brush::BrushDefaults;
pub use dabs::{paint_polygons_dabs, DabFillStyle};
pub use hokusai::color::RgbaF32;
pub use hokusai::Brush;
pub use strokes::{paint_lines, LineStrokeStyle};
#[cfg(feature = "parallel")]
pub use strokes::paint_lines_parallel;

use ezu_features::Polygon;
use tiny_skia::{
    Color, FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, PremultipliedColorU8, Stroke,
    Transform,
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
    pub fn new(tile_w: u32, tile_h: u32) -> Self {
        Self::new_padded(tile_w, tile_h, 0)
    }

    /// Create a canvas whose internal buffer is `tile_w + 2*pad` × `tile_h + 2*pad`.
    pub fn new_padded(tile_w: u32, tile_h: u32, pad: u32) -> Self {
        let pw = tile_w + 2 * pad;
        let ph = tile_h + 2 * pad;
        let pixmap = Pixmap::new(pw, ph).expect("non-zero canvas size");
        Self {
            pixmap,
            tile_w,
            tile_h,
            pad,
        }
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
    let mut layer = Pixmap::new(w, h).expect("non-zero layer");

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

    for poly in polygons {
        let Some(path) = build_polygon_path(poly, sx, sy, ox, oy) else {
            continue;
        };
        layer.fill_path(
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
            layer.stroke_path(&path, &edge_paint, &stroke, Transform::identity(), None);
        }
    }

    if style.blur_sigma > 0.0 {
        blur_pixmap(&mut layer, style.blur_sigma);
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
        *dst = PremultipliedColorU8::from_rgba(mul(r, a), mul(g, a), mul(b, a), a)
            .unwrap_or_else(|| PremultipliedColorU8::from_rgba(0, 0, 0, 0).unwrap());
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

