//! Font loading, coverage lookup, and glyph outline extraction — plus
//! the [`StackEntry`] wrapper that lets an outline [`Font`] and an SDF
//! glyph stack share one fallback stack.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use tiny_skia::{Path, PathBuilder};

use super::sdf::SdfFontStack;

/// Errors constructing a [`Font`] from raw bytes.
#[derive(Debug, thiserror::Error)]
pub enum FontError {
    #[error("font face #{index} failed to parse: {msg}")]
    Parse { index: u32, msg: String },
}

/// One immutable font face: the raw font-file bytes plus a per-glyph
/// outline cache. Cheap to share across threads behind an `Arc`.
///
/// The face structs of `ttf-parser` / `rustybuzz` borrow the byte slice
/// they were parsed from, which would make a struct holding both the
/// bytes and a parsed face self-referential. Rather than pulling in a
/// self-referential-struct crate, the face is re-created from the bytes
/// per operation batch ([`Font::with_face`]) — parsing is only a
/// table-directory scan (microseconds), negligible next to shaping or
/// rasterization.
///
/// Glyph outlines are extracted once per glyph id into a
/// [`tiny_skia::Path`] in *font units* (size-independent, y-up) and
/// cached behind an `RwLock`, so parallel tile renders share the
/// extraction cost.
pub struct Font {
    bytes: Arc<[u8]>,
    face_index: u32,
    units_per_em: f32,
    ascent_em: f32,
    descent_em: f32,
    glyph_paths: RwLock<HashMap<u16, Option<Arc<Path>>>>,
}

impl Font {
    /// Parse `bytes` (TTF / OTF / TTC) and build a font around face
    /// `face_index` (0 for single-face files). Validates that both the
    /// `ttf-parser` and `rustybuzz` views parse, so later per-batch face
    /// re-creation cannot fail.
    pub fn from_bytes(bytes: Arc<[u8]>, face_index: u32) -> Result<Font, FontError> {
        let face = ttf_parser::Face::parse(&bytes, face_index).map_err(|e| FontError::Parse {
            index: face_index,
            msg: e.to_string(),
        })?;
        let units_per_em = face.units_per_em() as f32;
        let ascent_em = face.ascender() as f32 / units_per_em;
        // `descender()` is negative (distance below the baseline);
        // store its magnitude.
        let descent_em = -(face.descender() as f32) / units_per_em;
        if rustybuzz::Face::from_slice(&bytes, face_index).is_none() {
            return Err(FontError::Parse {
                index: face_index,
                msg: "rustybuzz rejected the face".into(),
            });
        }
        Ok(Font {
            bytes,
            face_index,
            units_per_em,
            ascent_em,
            descent_em,
            glyph_paths: RwLock::new(HashMap::new()),
        })
    }

    /// Font units per em (typically 1000 or 2048).
    pub fn units_per_em(&self) -> f32 {
        self.units_per_em
    }

    /// Ascender above the baseline, in em.
    pub fn ascent_em(&self) -> f32 {
        self.ascent_em
    }

    /// Descender depth below the baseline, in em (positive magnitude).
    pub fn descent_em(&self) -> f32 {
        self.descent_em
    }

    /// Run `f` against a freshly parsed `rustybuzz::Face` (which derefs
    /// to `ttf_parser::Face`). Re-created per batch — see the type docs.
    pub(crate) fn with_face<R>(&self, f: impl FnOnce(&rustybuzz::Face<'_>) -> R) -> R {
        let face = rustybuzz::Face::from_slice(&self.bytes, self.face_index)
            .expect("face validated in Font::from_bytes");
        f(&face)
    }

    /// Whether this font's cmap covers `c`.
    pub fn covers(&self, c: char) -> bool {
        self.with_face(|face| face.glyph_index(c).is_some())
    }

    /// The glyph's outline as a `tiny_skia::Path` in font units (y-up),
    /// or `None` for glyphs without an outline (whitespace). Cached per
    /// glyph id.
    pub fn glyph_path(&self, glyph_id: u16) -> Option<Arc<Path>> {
        if let Some(cached) = self
            .glyph_paths
            .read()
            .expect("glyph cache poisoned")
            .get(&glyph_id)
        {
            return cached.clone();
        }
        let path = self.with_face(|face| {
            let mut builder = PathOutline {
                pb: PathBuilder::new(),
            };
            face.outline_glyph(ttf_parser::GlyphId(glyph_id), &mut builder)?;
            builder.pb.finish().map(Arc::new)
        });
        self.glyph_paths
            .write()
            .expect("glyph cache poisoned")
            .insert(glyph_id, path.clone());
        path
    }
}

/// One entry of a text fallback stack: an outline [`Font`] (real font
/// file, rustybuzz shaping) or an [`SdfFontStack`] (MapLibre glyph-PBF
/// ranges, fixed 24 px metrics). Both kinds mix freely in one stack;
/// itemization walks entries in order and the first one covering a
/// char wins, exactly as with a homogeneous stack.
#[derive(Debug, Clone)]
pub enum StackEntry {
    Outline(Arc<Font>),
    Sdf(Arc<SdfFontStack>),
}

impl StackEntry {
    /// Whether this entry can shape `c`. For an SDF stack this may
    /// fetch the char's range on demand (when a fetcher is present).
    pub(crate) fn covers(&self, c: char) -> bool {
        match self {
            StackEntry::Outline(f) => f.covers(c),
            StackEntry::Sdf(s) => s.coverage(c) == super::sdf::SdfCoverage::Present,
        }
    }
}

impl From<Arc<Font>> for StackEntry {
    fn from(f: Arc<Font>) -> Self {
        StackEntry::Outline(f)
    }
}

impl From<Arc<SdfFontStack>> for StackEntry {
    fn from(s: Arc<SdfFontStack>) -> Self {
        StackEntry::Sdf(s)
    }
}

impl std::fmt::Debug for Font {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Font")
            .field("face_index", &self.face_index)
            .field("units_per_em", &self.units_per_em)
            .finish()
    }
}

/// Adapts ttf-parser's outline callbacks onto a tiny-skia `PathBuilder`.
struct PathOutline {
    pb: PathBuilder,
}

impl ttf_parser::OutlineBuilder for PathOutline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.pb.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.pb.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.pb.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.pb.close();
    }
}
