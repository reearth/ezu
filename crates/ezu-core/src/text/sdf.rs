//! MapLibre glyph-PBF (SDF) font backend.
//!
//! MapLibre GL renders text not from font files but from pre-rendered
//! **signed-distance-field glyph bitmaps**, served in 256-codepoint
//! ranges from a `…/{fontstack}/{range}.pbf` endpoint (fontnik
//! protobufs, see [`super::pbf`]). [`SdfFontStack`] is the compat-mode
//! counterpart of [`Font`](super::Font): it accumulates decoded ranges
//! and shapes/draws with MapLibre's fixed metrics, so an ezu style can
//! label a map from a MapLibre `glyphs` endpoint with no font files.
//!
//! # Compat quirks (inherited from the protocol, kept for parity)
//!
//! - Glyphs are rasterized once at a **24 px em** ([`SDF_EM_PX`]);
//!   other sizes scale the SDF, so labels much larger than 24 px render
//!   soft compared to the outline backend.
//! - The field encodes 8 px of distance ([`SDF_RADIUS_PX`], cutoff
//!   0.25): the glyph edge sits at SDF value [`SDF_EDGE`] and the field
//!   reaches zero 6 px outside it, so halos saturate at 6 px at the
//!   24 px em — ¼ em, MapLibre's documented `text-halo-width` maximum.
//! - Line metrics don't consult real font metrics: every baseline sits
//!   at a fixed **−17 px** offset ([`SDF_Y_OFFSET_PX`]) within its line
//!   slot and the block is `line-height × line count` tall
//!   (maplibre-gl-js `shaping.ts`, `SHAPING_DEFAULT_OFFSET`).
//! - No kerning or ligatures — one codepoint maps to one glyph — and
//!   only the Basic Multilingual Plane is addressable by the range
//!   scheme; astral codepoints never resolve.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use xxhash_rust::xxh3::{xxh3_64, Xxh3};

use super::pbf::{decode_glyph_range, GlyphPbfError};

/// The em size every glyph PBF is rasterized at.
pub const SDF_EM_PX: f32 = 24.0;
/// Distance radius encoded by the SDF, in px at the 24 px em (the
/// shader's `SDF_PX`).
pub const SDF_RADIUS_PX: f32 = 8.0;
/// SDF value at the glyph edge (fontnik cutoff 0.25 → `1 − 0.25`).
pub const SDF_EDGE: f32 = 0.75;
/// Border baked around every glyph bitmap, in px — bitmap dimensions
/// are `(width + 2·border) × (height + 2·border)`.
pub const SDF_BORDER: u32 = 3;
/// Fixed per-line baseline offset MapLibre applies in SDF shaping
/// (`SHAPING_DEFAULT_OFFSET` in `shaping.ts`), in px at the 24 px em.
pub const SDF_Y_OFFSET_PX: f32 = -17.0;

/// One decoded SDF glyph. Metrics are in px at the 24 px em; `left` is
/// the ink-left bearing from the pen, `top` is the ink top relative to
/// the font's *ascender line* (fontnik writes `bitmap_top − ascender`,
/// so it is typically negative), `advance` the pen advance.
#[derive(Debug, Clone)]
pub struct SdfGlyph {
    pub id: u32,
    /// `(width+6) × (height+6)` SDF bytes, row-major; empty for inkless
    /// glyphs (spaces).
    pub bitmap: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub advance: u32,
}

/// Host-supplied callback fetching one raw range PBF by its codepoint
/// bounds (e.g. `(256, 511)` → `…/256-511.pbf`). Called lazily the
/// first time shaping needs a codepoint from the range.
pub type RangeFetcher = Box<dyn Fn(u32, u32) -> Result<Vec<u8>, String> + Send + Sync>;

/// One 256-codepoint range slot: decoded, or remembered as failed so a
/// broken range isn't refetched on every eval (the failure is part of
/// [`SdfFontStack::ranges_hash`], so caches reflect it).
enum RangeSlot {
    Loaded {
        /// BMP codepoint → glyph.
        glyphs: HashMap<u16, Arc<SdfGlyph>>,
        /// Content hash of the raw PBF bytes.
        hash: u64,
    },
    Failed,
}

/// A fontstack served as SDF glyph ranges — the `text` node's compat
/// counterpart of a [`Font`](super::Font) stack entry.
///
/// The range map is interior-mutable: ranges arrive either pushed by
/// the host up front ([`insert_range`](Self::insert_range), the wasm
/// path) or pulled on demand through an optional [`RangeFetcher`] (the
/// native path) the first time shaping needs a codepoint from an
/// unloaded range. Ranges only ever accumulate; a fetch failure is
/// remembered per range. [`ranges_hash`](Self::ranges_hash) digests
/// the loaded/failed set so asset consumers can key caches on exactly
/// what affects output.
pub struct SdfFontStack {
    ranges: RwLock<HashMap<u16, RangeSlot>>,
    fetcher: Option<RangeFetcher>,
}

impl SdfFontStack {
    /// A stack with no fetcher: every range must be pushed up front via
    /// [`insert_range`](Self::insert_range) (wasm hosts).
    pub fn new() -> Self {
        SdfFontStack {
            ranges: RwLock::new(HashMap::new()),
            fetcher: None,
        }
    }

    /// A stack that pulls missing ranges through `fetcher` on demand.
    pub fn with_fetcher(fetcher: RangeFetcher) -> Self {
        SdfFontStack {
            ranges: RwLock::new(HashMap::new()),
            fetcher: Some(fetcher),
        }
    }

    /// Whether missing ranges can be fetched on demand.
    pub fn has_fetcher(&self) -> bool {
        self.fetcher.is_some()
    }

    /// The range block (`codepoint >> 8`) covering `c`, or `None`
    /// outside the BMP (unreachable by the glyph protocol).
    pub fn block_of(c: char) -> Option<u16> {
        u16::try_from(c as u32).ok().map(|u| u >> 8)
    }

    /// Codepoint bounds of a range block: `(block·256, block·256+255)`,
    /// the numbers in the `{range}` URL slot.
    pub fn block_bounds(block: u16) -> (u32, u32) {
        let start = u32::from(block) << 8;
        (start, start + 255)
    }

    /// The distinct range blocks `text` needs, sorted. Non-BMP chars
    /// (which the protocol cannot serve) are omitted.
    pub fn blocks_for(text: &str) -> Vec<u16> {
        let mut blocks: Vec<u16> = text.chars().filter_map(Self::block_of).collect();
        blocks.sort_unstable();
        blocks.dedup();
        blocks
    }

    /// Whether `block` has been loaded (or its fetch has failed —
    /// either way, no further fetch will run for it).
    pub fn is_loaded(&self, block: u16) -> bool {
        self.ranges
            .read()
            .expect("range map poisoned")
            .contains_key(&block)
    }

    /// Ranges resolved so far (loaded or failed), and the glyph-bitmap
    /// bytes they hold. Ranges accumulate for the life of the stack, so
    /// this is what a long-lived host is paying to keep the fontstack
    /// resident — the number to look at when a render's memory is not
    /// accounted for by its pixel buffers.
    pub fn loaded_size(&self) -> (usize, usize) {
        let ranges = self.ranges.read().expect("range map poisoned");
        let bytes = ranges
            .values()
            .map(|slot| match slot {
                RangeSlot::Loaded { glyphs, .. } => {
                    glyphs.values().map(|g| g.bitmap.len()).sum::<usize>()
                }
                RangeSlot::Failed => 0,
            })
            .sum();
        (ranges.len(), bytes)
    }

    /// Decode one raw range PBF and store it under its own block (from
    /// the message's `range` field). Replaces any earlier slot.
    pub fn insert_range(&self, bytes: &[u8]) -> Result<(), GlyphPbfError> {
        let decoded = decode_glyph_range(bytes)?;
        let block = (decoded.start >> 8) as u16;
        let glyphs = decoded
            .glyphs
            .into_iter()
            .filter_map(|g| u16::try_from(g.id).ok().map(|id| (id, Arc::new(g))))
            .collect();
        self.ranges.write().expect("range map poisoned").insert(
            block,
            RangeSlot::Loaded {
                glyphs,
                hash: xxh3_64(bytes),
            },
        );
        Ok(())
    }

    /// Look up the glyph for `c`, fetching its range first if a fetcher
    /// is present and the range hasn't been seen. Returns `None` when
    /// the range has no such glyph — or when the range is unavailable
    /// (no fetcher / fetch failed), which [`coverage`](Self::coverage)
    /// distinguishes.
    pub fn glyph(&self, c: char) -> Option<Arc<SdfGlyph>> {
        let block = Self::block_of(c)?;
        self.ensure(block);
        match self.ranges.read().expect("range map poisoned").get(&block) {
            Some(RangeSlot::Loaded { glyphs, .. }) => glyphs.get(&(c as u16)).cloned(),
            _ => None,
        }
    }

    /// Coverage of `c`, fetching its range on demand like
    /// [`glyph`](Self::glyph).
    pub fn coverage(&self, c: char) -> SdfCoverage {
        let Some(block) = Self::block_of(c) else {
            return SdfCoverage::Absent;
        };
        self.ensure(block);
        match self.ranges.read().expect("range map poisoned").get(&block) {
            Some(RangeSlot::Loaded { glyphs, .. }) => {
                if glyphs.contains_key(&(c as u16)) {
                    SdfCoverage::Present
                } else {
                    SdfCoverage::Absent
                }
            }
            // Failed fetch, or never loaded and nothing to fetch with.
            _ => SdfCoverage::RangeUnavailable,
        }
    }

    /// Digest of the loaded/failed range set — everything that affects
    /// shaping output. Consumers fold this into cache keys so lazily
    /// grown ranges (or a fetch failure turning into a success) never
    /// produce stale hits.
    pub fn ranges_hash(&self) -> u128 {
        let map = self.ranges.read().expect("range map poisoned");
        let mut entries: Vec<(u16, u64)> = map
            .iter()
            .map(|(&block, slot)| match slot {
                RangeSlot::Loaded { hash, .. } => (block, *hash),
                RangeSlot::Failed => (block, u64::MAX),
            })
            .collect();
        entries.sort_unstable_by_key(|&(block, _)| block);
        let mut h = Xxh3::new();
        for (block, hash) in entries {
            h.update(&block.to_le_bytes());
            h.update(&hash.to_le_bytes());
        }
        h.digest128()
    }

    /// Fetch-and-insert `block` if it hasn't been seen and a fetcher is
    /// available. The fetch runs outside the lock; two threads racing
    /// on the same range insert identical content.
    fn ensure(&self, block: u16) {
        let Some(fetcher) = &self.fetcher else {
            return;
        };
        if self.is_loaded(block) {
            return;
        }
        let (start, end) = Self::block_bounds(block);
        let slot = match fetcher(start, end) {
            Ok(bytes) => match decode_glyph_range(&bytes) {
                Ok(_) => {
                    // Re-decode through the normal insert path so the
                    // slot layout has a single source of truth.
                    let _ = self.insert_range(&bytes);
                    return;
                }
                Err(e) => {
                    tracing::warn!("glyph range {start}-{end}: decode failed: {e}");
                    RangeSlot::Failed
                }
            },
            Err(e) => {
                tracing::warn!("glyph range {start}-{end}: fetch failed: {e}");
                RangeSlot::Failed
            }
        };
        self.ranges
            .write()
            .expect("range map poisoned")
            .insert(block, slot);
    }
}

impl Default for SdfFontStack {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SdfFontStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let map = self.ranges.read().expect("range map poisoned");
        f.debug_struct("SdfFontStack")
            .field("ranges", &map.len())
            .field("fetcher", &self.fetcher.is_some())
            .finish()
    }
}

/// Result of an [`SdfFontStack::coverage`] probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdfCoverage {
    /// The glyph is present in its (loaded) range.
    Present,
    /// The range is loaded but has no such glyph (or the codepoint is
    /// outside the BMP).
    Absent,
    /// The range could not be consulted: never loaded and no fetcher,
    /// or its fetch failed. Callers surface this distinctly so a host
    /// that must pre-bind ranges (wasm) gets an actionable warning.
    RangeUnavailable,
}
