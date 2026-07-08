//! Font loading, coverage lookup, and glyph outline extraction — plus
//! the [`StackEntry`] wrapper that lets an outline [`Font`] and an SDF
//! glyph stack share one fallback stack, and the [`FaceEntry`] view that
//! carries each outline entry's already-built [`rustybuzz::Face`] so
//! shaping, coverage, and outline lookups reuse it instead of reparsing.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use tiny_skia::{Path, PathBuilder};
use xxhash_rust::xxh3::Xxh3;

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
/// self-referential-struct crate, callers build a [`rustybuzz::Face`]
/// from the bytes with [`Font::face`] and pass it back in (shaping,
/// coverage, and outline extraction all take a `&Face`). Building the
/// face parses the table directory plus the shaping tables — the
/// dominant cost of a text node — so a caller that lays out many labels
/// against one stack should build each face once and reuse it (see
/// [`FaceEntry`]) rather than per operation. `rustybuzz::Face` is cheap
/// to `clone` (it copies the already-parsed tables, no re-scan), so one
/// built face can be shared by value across every label that uses it.
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
    /// A stable content fingerprint of `(bytes, face_index)`, computed once
    /// at construction. Unlike an `Arc<Font>` pointer — stable only for one
    /// allocation's lifetime and reusable across evals — this keys the two
    /// fonts loaded from identical bytes to a single entry in the
    /// process-wide glyph-SDF and shaped-layout caches.
    content_hash: u64,
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
        let mut hasher = Xxh3::new();
        hasher.update(&face_index.to_le_bytes());
        hasher.update(&bytes);
        let content_hash = hasher.digest();
        Ok(Font {
            bytes,
            face_index,
            units_per_em,
            ascent_em,
            descent_em,
            content_hash,
            glyph_paths: RwLock::new(HashMap::new()),
        })
    }

    /// A stable content fingerprint of this font's bytes and face index,
    /// used to key the process-wide glyph-SDF and shaped-layout caches so
    /// two `Font`s parsed from identical bytes share cache entries.
    pub fn content_hash(&self) -> u64 {
        self.content_hash
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

    /// Build a `rustybuzz::Face` (which derefs to `ttf_parser::Face`)
    /// borrowing this font's bytes. Validated in [`Font::from_bytes`], so
    /// it cannot fail here. See the type docs on reusing one built face.
    pub fn face(&self) -> rustybuzz::Face<'_> {
        rustybuzz::Face::from_slice(&self.bytes, self.face_index)
            .expect("face validated in Font::from_bytes")
    }

    /// Whether this font's cmap covers `c`, using a caller-built `face` so
    /// no reparse happens per lookup.
    pub fn covers(&self, face: &rustybuzz::Face<'_>, c: char) -> bool {
        face.glyph_index(c).is_some()
    }

    /// The glyph's outline as a `tiny_skia::Path` in font units (y-up),
    /// or `None` for glyphs without an outline (whitespace). Cached per
    /// glyph id; the caller-built `face` is only touched on a cache miss.
    pub fn glyph_path(&self, face: &rustybuzz::Face<'_>, glyph_id: u16) -> Option<Arc<Path>> {
        if let Some(cached) = self
            .glyph_paths
            .read()
            .expect("glyph cache poisoned")
            .get(&glyph_id)
        {
            return cached.clone();
        }
        let path = {
            let mut builder = PathOutline {
                pb: PathBuilder::new(),
            };
            face.outline_glyph(ttf_parser::GlyphId(glyph_id), &mut builder)
                .and_then(|_| builder.pb.finish().map(Arc::new))
        };
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

/// A [`StackEntry`] prepared for shaping/drawing: an outline entry with its
/// [`rustybuzz::Face`] already built, or an SDF stack (which needs no face).
/// Shaping, coverage, and outline extraction all take a slice of these so
/// the face is built once and reused, instead of reparsed per call.
///
/// The face is held by value; it is a cheap clone of a parsed face (see the
/// [`Font`] docs), so one built face can be spread across every prepared
/// stack that references its font.
///
/// The outline variant is large because it embeds the parsed face; the size
/// disparity with the SDF variant is deliberate and harmless here — these
/// live briefly in short (per-label) stacks, and holding the face by value is
/// what lets it be a plain copy rather than a heap indirection.
#[allow(clippy::large_enum_variant)]
pub enum FaceEntry<'a> {
    Outline {
        font: &'a Font,
        face: rustybuzz::Face<'a>,
    },
    Sdf(&'a SdfFontStack),
}

impl<'a> FaceEntry<'a> {
    /// Prepare a whole fallback stack, building each outline entry's face
    /// once. Callers that lay out many labels over stacks sharing fonts
    /// should build faces once (e.g. keyed by font) and assemble views from
    /// those clones, rather than calling this per label.
    pub fn prepare(stack: &'a [StackEntry]) -> Vec<FaceEntry<'a>> {
        stack.iter().map(FaceEntry::from_entry).collect()
    }

    /// Prepare a single entry, building its face if it is an outline font.
    pub fn from_entry(entry: &'a StackEntry) -> FaceEntry<'a> {
        match entry {
            StackEntry::Outline(f) => FaceEntry::Outline {
                font: f,
                face: f.face(),
            },
            StackEntry::Sdf(s) => FaceEntry::Sdf(s),
        }
    }

    /// The nominal horizontal advance (em) this entry uses for `c`, taken
    /// straight from cmap+hmtx (outline) or the glyph PBF (SDF), or `None` when
    /// the entry does not cover `c`. This is the pre-shaping per-glyph advance,
    /// before kerning or ligature adjustment, so summed across a label it
    /// over-estimates the shaped width — a caller can scale it down into a
    /// cheap, shaping-free lower bound on the label's extent.
    pub fn advance_em(&self, c: char) -> Option<f32> {
        match self {
            FaceEntry::Outline { font, face } => {
                let gid = face.glyph_index(c)?;
                let adv = face.glyph_hor_advance(gid)?;
                Some(adv as f32 / font.units_per_em())
            }
            FaceEntry::Sdf(s) => Some(s.glyph(c)?.advance as f32 / super::sdf::SDF_EM_PX),
        }
    }

    /// Whether this entry can shape `c`. For an SDF stack this may fetch the
    /// char's range on demand (when a fetcher is present).
    pub(crate) fn covers(&self, c: char) -> bool {
        match self {
            FaceEntry::Outline { font, face } => font.covers(face, c),
            FaceEntry::Sdf(s) => s.coverage(c) == super::sdf::SdfCoverage::Present,
        }
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
