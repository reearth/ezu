//! Text shaping, layout, and drawing (feature `text`).
//!
//! Pure, tile-agnostic text machinery in the MapLibre mold, built on
//! `rustybuzz` (shaping) + `ttf-parser` (outlines) + `tiny-skia`
//! (rasterization) — no system-font access, so output is deterministic
//! and identical across native and wasm hosts.
//!
//! Two font backends share one fallback stack ([`StackEntry`]):
//!
//! - **Outline** ([`Font`]) — a real font file; rustybuzz shaping with
//!   kerning/ligatures, vector-path rasterization at any size.
//! - **SDF** ([`SdfFontStack`]) — MapLibre glyph-PBF ranges from a
//!   `glyphs` endpoint; 1 codepoint → 1 pre-rendered 24 px SDF bitmap,
//!   MapLibre's fixed metrics. Lower fidelity, zero font files — see
//!   [`sdf`] for the protocol constants and compat quirks. Arabic joins
//!   through the Unicode presentation forms, the one shaping a
//!   glyph stack can express.
//!
//! The pipeline is split in two so shaping cost is paid once per label
//! and drawing once per placement:
//!
//! 1. [`layout`] — transform / resolve bidi / itemize / shape /
//!    line-break / reorder / anchor a string against the fallback
//!    stack, producing a size-independent [`TextBlock`] in em units.
//!    Its glyphs come out in visual order, left to right.
//! 2. [`draw`] — rasterize a [`TextBlock`] onto a `tiny_skia::PixmapMut`
//!    at a given origin and font size, with fill color and optional halo.
//!
//! MapLibre `format` sections are supported: [`layout_sections`] lays out
//! per-section font (a subrange of the stack), `font-scale`, and
//! `vertical-align` (see [`VerticalAlign`]); a line's metrics scale by its
//! tallest section. [`draw`] takes a per-section fill table (see
//! [`SectionPaint`]).
//!
//! # Scope
//!
//! Deliberately not handled; callers should not expect them:
//!
//! - vertical writing mode
//! - color-emoji tables (`COLR` / `CBDT`) — such glyphs draw as their
//!   monochrome outline, or not at all if the font has none

mod arabic;
mod bidi;
pub mod collide;
mod draw;
mod font;
mod layout;
pub mod layout_cache;
pub mod line;
pub mod outline_sdf;
pub mod pbf;
pub mod sdf;
mod shape;

pub use collide::{
    place, place_layers, Aabb, Grid, LabelCandidate, PlaceRank, Placement, COLLISION_CELL_PX,
    DEDUP_QUANTUM,
};
pub use draw::{draw, draw_line, GlyphPlacement, SectionPaint, TextPaint};
pub use font::{FaceEntry, Font, FontError, StackEntry};
pub use layout::{
    char_allows_ideographic_breaking, layout, layout_sections, Anchor, EmBox, Justify,
    LayoutParams, PlacedGlyph, SectionSpec, TextBlock, TextTransform, VerticalAlign,
};
pub use layout_cache::get_or_build_layout;
pub use line::{
    clip_line, generate_anchors, place_glyphs, Anchor as LineAnchor, AnchorParams, GlyphOnLine,
    LinePlacement,
};
pub use outline_sdf::{OutlineSdfCache, OutlineSdfStats};
pub use pbf::{decode_glyph_range, GlyphPbfError, GlyphRange};
pub use sdf::{RangeFetcher, SdfCoverage, SdfFontStack, SdfGlyph};
