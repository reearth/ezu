//! Render an outline glyph into a MapLibre-compatible SDF bitmap so it
//! can draw through the same field-sampling path as glyph-PBF text (see
//! [`super::draw`] and [`super::sdf`]).
//!
//! The bitmap is generated the way MapLibre's client-side glyph generator
//! (`tiny-sdf`) does: rasterize the glyph to a coverage mask at the 24 px
//! em, run a Felzenszwalb–Huttenlocher exact Euclidean distance transform
//! over the inside and outside, and encode the signed distance with the
//! same radius / cutoff the shader decodes. The result is an [`SdfGlyph`]
//! in the [`super::sdf`] encoding (edge at [`super::sdf::SDF_EDGE`], a
//! [`SDF_BORDER`]-px skirt), with metrics referenced to the outline
//! backend's baseline pen so [`super::draw::draw`] places it identically
//! to a vector fill.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use tiny_skia::{Color, FillRule, Paint, Pixmap, Transform};

use super::font::Font;
use super::sdf::{SdfGlyph, SDF_BORDER, SDF_EM_PX, SDF_RADIUS_PX};

/// Memoizes [`build`] per `(font identity, glyph id)` for the span of one
/// text-node eval, so an outline glyph is SDF-rasterized once no matter how
/// many labels or placements repeat it. SDF bitmaps are size-independent,
/// so size is not part of the key.
///
/// The key's font component is the `Arc<Font>`'s pointer identity: every
/// label's flat stack is cloned from the registry's shared fonts, so the
/// same glyph across labels hits one entry. Interior-mutable so the shared
/// `&self` draw path can fill it; a `None` value memoizes "this glyph has
/// no outline" so it is not re-attempted.
/// Cache key: the `Arc<Font>`'s pointer identity paired with a glyph id.
type GlyphKey = (usize, u16);

#[derive(Default)]
pub struct OutlineSdfCache {
    glyphs: RwLock<HashMap<GlyphKey, Option<Arc<SdfGlyph>>>>,
    hits: AtomicU64,
    misses: AtomicU64,
}

/// Cache tallies: unique glyphs built (misses), reuse count (hits), and the
/// bytes held by the built SDF bitmaps.
#[derive(Debug, Clone, Copy)]
pub struct OutlineSdfStats {
    pub built: u64,
    pub hits: u64,
    pub bitmap_bytes: usize,
}

impl OutlineSdfCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The SDF for `glyph_id` of `font`, building and caching it on first
    /// use. `None` for a glyph with no outline (whitespace / degenerate).
    pub fn get(
        &self,
        font: &Font,
        face: &rustybuzz::Face<'_>,
        glyph_id: u16,
    ) -> Option<Arc<SdfGlyph>> {
        let key: GlyphKey = (font as *const Font as usize, glyph_id);
        if let Some(hit) = self.glyphs.read().expect("sdf cache poisoned").get(&key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return hit.clone();
        }
        // Built outside the write lock; a race just rebuilds identically.
        let built = build(font, face, glyph_id).map(Arc::new);
        self.misses.fetch_add(1, Ordering::Relaxed);
        self.glyphs
            .write()
            .expect("sdf cache poisoned")
            .insert(key, built.clone());
        built
    }

    /// A snapshot of cache activity for reporting.
    pub fn stats(&self) -> OutlineSdfStats {
        let map = self.glyphs.read().expect("sdf cache poisoned");
        let bitmap_bytes = map
            .values()
            .filter_map(|g| g.as_ref())
            .map(|g| g.bitmap.len())
            .sum();
        OutlineSdfStats {
            built: self.misses.load(Ordering::Relaxed),
            hits: self.hits.load(Ordering::Relaxed),
            bitmap_bytes,
        }
    }
}

/// Distance-field cutoff: the SDF value at the glyph edge is `1 - CUTOFF`
/// (i.e. [`super::sdf::SDF_EDGE`]). Matches fontnik / tiny-sdf.
const CUTOFF: f32 = 0.25;

/// Large finite squared distance used to seed empty / full cells before
/// the distance transform (`tiny-sdf`'s `INF`).
const INF: f32 = 1e20;

/// Rasterize outline glyph `glyph_id` of `font` into a MapLibre-compatible
/// SDF bitmap at the 24 px em. Returns `None` for a glyph with no outline
/// (whitespace) or a degenerate ink box; the caller skips those exactly as
/// it skips an outline glyph with no path.
pub fn build(font: &Font, face: &rustybuzz::Face<'_>, glyph_id: u16) -> Option<SdfGlyph> {
    let path = font.glyph_path(face, glyph_id)?;
    let upm = font.units_per_em();
    // Font units (y-up) → px at the 24 px em.
    let scale = SDF_EM_PX / upm;

    // Ink box in device px (y-down, baseline at y = 0). tiny-skia's
    // `bounds()` is a control-point box — never tighter than the ink, so
    // the glyph always fits inside the padded bitmap; a little slack only
    // costs a few edge texels of field.
    let b = path.bounds();
    let x_lo = b.left() * scale;
    let x_hi = b.right() * scale;
    // y-up `bottom()` is the highest ink; flip to y-down.
    let y_lo = -b.bottom() * scale;
    let y_hi = -b.top() * scale;

    // Snap the ink box out to integer px: this pins the bitmap to a pixel
    // grid, and the metrics below reference that grid so the draw step
    // reproduces the vector fill's placement (the sub-pixel edge lives in
    // the coverage the transform bakes in).
    let ix0 = x_lo.floor() as i32;
    let iy0 = y_lo.floor() as i32;
    let ix1 = x_hi.ceil() as i32;
    let iy1 = y_hi.ceil() as i32;
    let gw = (ix1 - ix0).max(0) as u32;
    let gh = (iy1 - iy0).max(0) as u32;
    if gw == 0 || gh == 0 {
        return None;
    }

    let border = SDF_BORDER as i32;
    let bw = gw + 2 * SDF_BORDER;
    let bh = gh + 2 * SDF_BORDER;

    // Rasterize a coverage mask: map font units so device px `ix0`/`iy0`
    // (the bitmap's ink corner) land at column/row `border`.
    let mut mask = Pixmap::new(bw, bh)?;
    let mut paint = Paint::default();
    paint.set_color(Color::WHITE);
    paint.anti_alias = true;
    let to_bitmap = Transform::from_row(
        scale,
        0.0,
        0.0,
        -scale,
        border as f32 - ix0 as f32,
        border as f32 - iy0 as f32,
    );
    mask.fill_path(&path, &paint, FillRule::Winding, to_bitmap, None);

    // Coverage → seed grids for the inside / outside distance transforms.
    // The tiny-sdf split: a fully-covered cell has zero outside distance,
    // an empty cell zero inside distance, and a partly-covered edge cell
    // seeds a sub-cell distance from its coverage so the zero-crossing
    // lands on the anti-aliased edge.
    let len = (bw * bh) as usize;
    let mut outer = vec![0f32; len];
    let mut inner = vec![0f32; len];
    for (i, px) in mask.pixels().iter().enumerate() {
        let a = px.alpha() as f32 / 255.0;
        if a >= 1.0 {
            outer[i] = 0.0;
            inner[i] = INF;
        } else if a <= 0.0 {
            outer[i] = INF;
            inner[i] = 0.0;
        } else {
            let o = (0.5 - a).max(0.0);
            let n = (a - 0.5).max(0.0);
            outer[i] = o * o;
            inner[i] = n * n;
        }
    }
    edt(&mut outer, bw as usize, bh as usize);
    edt(&mut inner, bw as usize, bh as usize);

    let radius = SDF_RADIUS_PX;
    let bitmap: Vec<u8> = (0..len)
        .map(|i| {
            let d = outer[i].sqrt() - inner[i].sqrt();
            let v = 255.0 - 255.0 * (d / radius + CUTOFF);
            v.round().clamp(0.0, 255.0) as u8
        })
        .collect();

    // Metrics referenced to the outline backend's baseline pen: `left` is
    // the ink-left bearing and `top` the ink ascent above the baseline,
    // both in 24 px-em px. The advance is unused at draw time (the outline
    // layout carries shaped advances) but filled from hmtx for completeness.
    let advance = face
        .glyph_hor_advance(ttf_parser::GlyphId(glyph_id))
        .map(|a| (a as f32 * scale).round().max(0.0) as u32)
        .unwrap_or(0);
    Some(SdfGlyph {
        id: u32::from(glyph_id),
        bitmap,
        width: gw,
        height: gh,
        left: ix0,
        top: -iy0,
        advance,
    })
}

/// In-place exact Euclidean distance transform of a squared-distance grid
/// (Felzenszwalb & Huttenlocher 2012), one pass down columns then across
/// rows — the algorithm `tiny-sdf` uses. `grid[i]` holds a squared seed
/// distance; on return it holds the squared distance to the nearest seed.
fn edt(grid: &mut [f32], width: usize, height: usize) {
    let max = width.max(height);
    let mut f = vec![0f32; max];
    let mut v = vec![0usize; max];
    let mut z = vec![0f32; max + 1];
    for x in 0..width {
        edt1d(grid, x, width, height, &mut f, &mut v, &mut z);
    }
    for y in 0..height {
        edt1d(grid, y * width, 1, width, &mut f, &mut v, &mut z);
    }
}

/// One-dimensional squared-distance transform along a strided lane of
/// `grid` (`length` samples from `offset`, step `stride`). `f`/`v`/`z` are
/// caller-provided scratch (lower-envelope parabola heights, vertices, and
/// break points) reused across lanes.
fn edt1d(
    grid: &mut [f32],
    offset: usize,
    stride: usize,
    length: usize,
    f: &mut [f32],
    v: &mut [usize],
    z: &mut [f32],
) {
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    f[0] = grid[offset];
    let mut k = 0usize;
    for q in 1..length {
        f[q] = grid[offset + q * stride];
        let q2 = (q * q) as f32;
        loop {
            let r = v[k];
            let s = (f[q] - f[r] + q2 - (r * r) as f32) / (q - r) as f32 / 2.0;
            if s <= z[k] {
                // Pop the last vertex; if it was the seed (`k == 0`) it is
                // replaced outright, matching tiny-sdf's `--k > -1` guard.
                if k == 0 {
                    v[0] = q;
                    z[0] = f32::NEG_INFINITY;
                    z[1] = f32::INFINITY;
                    break;
                }
                k -= 1;
            } else {
                k += 1;
                v[k] = q;
                z[k] = s;
                z[k + 1] = f32::INFINITY;
                break;
            }
        }
    }
    k = 0;
    for q in 0..length {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let r = v[k];
        let d = q as f32 - r as f32;
        grid[offset + q * stride] = d * d + f[r];
    }
}
