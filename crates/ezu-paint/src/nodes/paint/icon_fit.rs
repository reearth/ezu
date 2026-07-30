//! MapLibre `icon-text-fit`: stretch a symbol's icon to the box its label
//! occupies.
//!
//! Two pieces, both pure:
//!
//! 1. [`fitted_content_box`] — where the icon's *content* area lands once it
//!    has been fitted to the placed text plus `icon-text-fit-padding`. The
//!    icon is centred on the text on any axis the fit doesn't name, exactly
//!    as the reference does (`icon-anchor` is deliberately ignored once a
//!    fit is asked for).
//! 2. [`stretch_image`] — nine-slice the sprite so only the bands the sprite
//!    index marks `stretchX` / `stretchY` absorb the growth, leaving a
//!    shield's rounded corners undistorted. A sprite with no stretch bands
//!    scales as a whole.
//!
//! Both work in canvas pixels, with `scale` (icon size over the sprite's
//! pixel ratio) converting sprite pixels into them.

use ezu_core::text::Aabb;
use ezu_graph::RasterBuf;
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, PixmapRef, Transform};

/// MapLibre `icon-text-fit`: which axes of the icon follow the label's box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub(super) enum IconTextFit {
    #[default]
    None,
    Width,
    Height,
    Both,
}

impl IconTextFit {
    /// Parse the MapLibre value; unknown names fall back to `none`.
    pub(super) fn parse(s: &str) -> Option<IconTextFit> {
        Some(match s {
            "none" => IconTextFit::None,
            "width" => IconTextFit::Width,
            "height" => IconTextFit::Height,
            "both" => IconTextFit::Both,
            _ => return None,
        })
    }

    fn fits_width(self) -> bool {
        matches!(self, IconTextFit::Width | IconTextFit::Both)
    }

    fn fits_height(self) -> bool {
        matches!(self, IconTextFit::Height | IconTextFit::Both)
    }
}

/// Where the icon's content area lands, relative to the label anchor (px).
///
/// `text` is the placed label's box in the same frame, `display` the sprite's
/// natural display size, `padding` MapLibre's `icon-text-fit-padding`
/// `[top, right, bottom, left]`, and `offset` the symbol's `icon-offset`.
/// A fitted axis spans the text plus its padding; an unfitted one keeps the
/// sprite's size, centred on the text.
pub(super) fn fitted_content_box(
    fit: IconTextFit,
    text: Aabb,
    display: (f32, f32),
    padding: [f32; 4],
    offset: (f32, f32),
) -> Aabb {
    let (min_x, max_x) = if fit.fits_width() {
        (
            offset.0 + text.min_x - padding[3],
            offset.0 + text.max_x + padding[1],
        )
    } else {
        let left = offset.0 + 0.5 * (text.min_x + text.max_x - display.0);
        (left, left + display.0)
    };
    let (min_y, max_y) = if fit.fits_height() {
        (
            offset.1 + text.min_y - padding[0],
            offset.1 + text.max_y + padding[2],
        )
    } else {
        let top = offset.1 + 0.5 * (text.min_y + text.max_y - display.1);
        (top, top + display.1)
    };
    Aabb {
        min_x,
        min_y,
        max_x,
        max_y,
    }
}

/// One axis of the nine-slice: the sprite's stretchable bands mapped onto the
/// destination, so a sprite pixel `u` becomes a canvas offset.
struct Axis {
    /// Band boundaries in sprite pixels, ascending, spanning `0..=len`.
    cuts: Vec<f32>,
    /// The destination offset (px) of each cut, relative to the drawn image's
    /// own left (resp. top) edge.
    dst: Vec<f32>,
}

impl Axis {
    /// Build the mapping for one axis. `bands` are the `[from, to)` sprite
    /// ranges free to stretch, `content` the sprite range that must land on
    /// `[0, target]`, and `scale` converts an unstretched sprite pixel.
    ///
    /// The stretch is shared over the bands in proportion to their widths;
    /// everything outside them keeps its natural size and simply rides along,
    /// so a shield's border stays crisp however wide the label is. Bands with
    /// no width left to give (or none at all) fall back to scaling the whole
    /// axis, the behaviour of a sprite without nine-slice metadata.
    fn new(len: u32, bands: &[[u32; 2]], content: (f32, f32), target: f32, scale: f32) -> Axis {
        let len = len as f32;
        let mut cuts = vec![0.0f32];
        for b in bands {
            let (a, z) = (b[0] as f32, b[1] as f32);
            if z <= a || a >= len {
                continue;
            }
            cuts.push(a.clamp(0.0, len));
            cuts.push(z.clamp(0.0, len));
        }
        cuts.push(len);
        cuts.sort_by(f32::total_cmp);
        cuts.dedup();
        // Sprite pixels inside a stretch band, up to `u`.
        let stretched = |u: f32| -> f32 {
            bands
                .iter()
                .map(|b| {
                    let (a, z) = (b[0] as f32, b[1] as f32);
                    (u.min(z) - u.min(a)).max(0.0)
                })
                .sum()
        };
        let (c0, c1) = content;
        let stretch_span = stretched(c1) - stretched(c0);
        let fixed_span = (c1 - c0) - stretch_span;
        // Destination px per stretched sprite px: whatever the fixed parts of
        // the content leave over.
        let growth = if stretch_span > 0.0 {
            ((target - fixed_span * scale) / stretch_span).max(0.0)
        } else {
            // Nothing may stretch, so the content scales as a whole.
            (target / (c1 - c0).max(f32::EPSILON)).max(0.0)
        };
        let at = |u: f32| {
            let (st, fx) = (stretched(u), u - stretched(u));
            let (st0, fx0) = (stretched(c0), c0 - stretched(c0));
            if stretch_span > 0.0 {
                (st - st0) * growth + (fx - fx0) * scale
            } else {
                (u - c0) * growth
            }
        };
        let origin = at(0.0);
        let dst = cuts.iter().map(|&u| at(u) - origin).collect();
        Axis { cuts, dst }
    }

    /// Where the content range starts, relative to the drawn image's edge.
    fn content_start(&self, content_from: f32) -> f32 {
        // The mapping is piecewise linear through the cuts; the content edge
        // is always one of them or inside a band.
        let mut i = 0;
        while i + 2 < self.cuts.len() && self.cuts[i + 1] <= content_from {
            i += 1;
        }
        let (u0, u1) = (self.cuts[i], self.cuts[i + 1]);
        let t = if u1 > u0 {
            (content_from - u0) / (u1 - u0)
        } else {
            0.0
        };
        self.dst[i] + (self.dst[i + 1] - self.dst[i]) * t
    }

    /// The drawn extent of this axis (px).
    fn extent(&self) -> f32 {
        *self.dst.last().unwrap_or(&0.0)
    }
}

/// The sprite's nine-slice bands and content box, in sprite pixels. Empty
/// bands mean the whole image stretches; an absent content box means the
/// whole image is the content.
pub(super) struct NineSlice<'a> {
    pub stretch_x: &'a [[u32; 2]],
    pub stretch_y: &'a [[u32; 2]],
    pub content: Option<[u32; 4]>,
}

/// A sprite stretched so its content area covers `target` (px).
///
/// Returns the drawn image and where its top-left sits relative to the
/// content box's top-left — the fixed margins outside the content keep their
/// natural size, so the drawn image usually overhangs the target.
pub(super) fn stretch_image(
    src: &RasterBuf,
    slice: &NineSlice<'_>,
    target: (f32, f32),
    scale: f32,
) -> Option<(RasterBuf, (f32, f32))> {
    let (sw, sh) = (src.width, src.height);
    if sw == 0 || sh == 0 {
        return None;
    }
    let content = slice.content.unwrap_or([0, 0, sw, sh]).map(|v| v as f32);
    let ax = Axis::new(
        sw,
        slice.stretch_x,
        (content[0], content[2]),
        target.0.max(0.0),
        scale,
    );
    let ay = Axis::new(
        sh,
        slice.stretch_y,
        (content[1], content[3]),
        target.1.max(0.0),
        scale,
    );
    let (dw, dh) = (ax.extent(), ay.extent());
    let mut out = Pixmap::new(dw.ceil().max(1.0) as u32, dh.ceil().max(1.0) as u32)?;
    let paint = PixmapPaint {
        quality: FilterQuality::Bilinear,
        ..PixmapPaint::default()
    };
    for xi in 0..ax.cuts.len() - 1 {
        for yi in 0..ay.cuts.len() - 1 {
            let (u0, u1) = (ax.cuts[xi], ax.cuts[xi + 1]);
            let (v0, v1) = (ay.cuts[yi], ay.cuts[yi + 1]);
            let (x0, x1) = (ax.dst[xi], ax.dst[xi + 1]);
            let (y0, y1) = (ay.dst[yi], ay.dst[yi + 1]);
            if u1 <= u0 || v1 <= v0 || x1 <= x0 || y1 <= y0 {
                continue;
            }
            // One slice: crop the sprite band, then scale it onto its share
            // of the destination.
            let tile = crop(
                src,
                u0 as u32,
                v0 as u32,
                (u1 - u0) as u32,
                (v1 - v0) as u32,
            )?;
            let tile_ref = PixmapRef::from_bytes(&tile.pixels, tile.width, tile.height)?;
            let t = Transform::from_translate(x0, y0).pre_scale(
                (x1 - x0) / tile.width as f32,
                (y1 - y0) / tile.height as f32,
            );
            out.draw_pixmap(0, 0, tile_ref, &paint, t, None);
        }
    }
    let origin = (-ax.content_start(content[0]), -ay.content_start(content[1]));
    Some((
        RasterBuf {
            width: out.width(),
            height: out.height(),
            pixels: out.take(),
        },
        origin,
    ))
}

/// A sub-rectangle of `src` as its own buffer.
fn crop(src: &RasterBuf, x: u32, y: u32, w: u32, h: u32) -> Option<RasterBuf> {
    let (w, h) = (
        w.min(src.width.saturating_sub(x)),
        h.min(src.height.saturating_sub(y)),
    );
    if w == 0 || h == 0 {
        return None;
    }
    let mut out = RasterBuf::new(w, h);
    for row in 0..h {
        let s = (((y + row) * src.width) + x) as usize * 4;
        let d = (row * w) as usize * 4;
        let n = w as usize * 4;
        out.pixels[d..d + n].copy_from_slice(&src.pixels[s..s + n]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: Aabb = Aabb {
        min_x: -30.0,
        min_y: -8.0,
        max_x: 30.0,
        max_y: 8.0,
    };

    #[test]
    fn an_unfitted_axis_keeps_the_sprite_size_centred_on_the_text() {
        let b = fitted_content_box(IconTextFit::None, TEXT, (20.0, 10.0), [0.0; 4], (0.0, 0.0));
        assert_eq!((b.max_x - b.min_x, b.max_y - b.min_y), (20.0, 10.0));
        // Centred on the text box, which is itself centred on the anchor.
        assert_eq!((b.min_x, b.min_y), (-10.0, -5.0));
    }

    #[test]
    fn a_fitted_axis_spans_the_text_plus_its_padding() {
        let pad = [1.0, 2.0, 3.0, 4.0];
        let b = fitted_content_box(IconTextFit::Both, TEXT, (20.0, 10.0), pad, (0.0, 0.0));
        assert_eq!((b.min_x, b.max_x), (-34.0, 32.0));
        assert_eq!((b.min_y, b.max_y), (-9.0, 11.0));
        // `width` leaves the vertical axis at the sprite's own height.
        let w = fitted_content_box(IconTextFit::Width, TEXT, (20.0, 10.0), pad, (0.0, 0.0));
        assert_eq!((w.min_x, w.max_x), (-34.0, 32.0));
        assert_eq!(w.max_y - w.min_y, 10.0);
        // The icon offset moves the whole box.
        let o = fitted_content_box(IconTextFit::Both, TEXT, (20.0, 10.0), pad, (5.0, -5.0));
        assert_eq!((o.min_x, o.min_y), (-29.0, -14.0));
    }

    /// A 10×1 sprite whose middle two columns stretch, each column a distinct
    /// alpha so the slicing is visible in the output.
    fn banded() -> RasterBuf {
        let mut buf = RasterBuf::new(10, 1);
        for x in 0..10u32 {
            buf.pixels[x as usize * 4 + 3] = (x as u8 + 1) * 20;
        }
        buf
    }

    #[test]
    fn only_the_stretch_bands_absorb_the_growth() {
        let slice = NineSlice {
            stretch_x: &[[4, 6]],
            stretch_y: &[],
            content: None,
        };
        let (img, origin) = stretch_image(&banded(), &slice, (30.0, 1.0), 1.0).unwrap();
        assert_eq!((img.width, img.height), (30, 1));
        // The content is the whole image, so it starts at the drawn edge.
        assert_eq!(origin, (0.0, 0.0));
        // The four fixed columns on each side keep their width; the two
        // stretch columns grew from 2 px to 22.
        assert_eq!(img.pixel(0, 0)[3], 20);
        assert_eq!(img.pixel(3, 0)[3], 80);
        assert_eq!(img.pixel(29, 0)[3], 200);
        assert_eq!(img.pixel(26, 0)[3], 140);
    }

    #[test]
    fn a_sprite_without_bands_scales_whole() {
        let slice = NineSlice {
            stretch_x: &[],
            stretch_y: &[],
            content: None,
        };
        let (img, _) = stretch_image(&banded(), &slice, (20.0, 2.0), 1.0).unwrap();
        assert_eq!((img.width, img.height), (20, 2));
    }

    #[test]
    fn the_content_box_lands_on_the_target() {
        // A 10 px sprite with a 2 px border either side: fitting the 6 px
        // content to 26 px draws 30 px, starting 2 px left of the target.
        let slice = NineSlice {
            stretch_x: &[[2, 8]],
            stretch_y: &[],
            content: Some([2, 0, 8, 1]),
        };
        let (img, origin) = stretch_image(&banded(), &slice, (26.0, 1.0), 1.0).unwrap();
        assert_eq!(img.width, 30);
        assert_eq!(origin.0, -2.0);
    }
}
