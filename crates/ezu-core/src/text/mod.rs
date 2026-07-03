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
//!   [`sdf`] for the protocol constants and compat quirks.
//!
//! The pipeline is split in two so shaping cost is paid once per label
//! and drawing once per placement:
//!
//! 1. [`layout`] — transform / itemize / shape / line-break / anchor a
//!    string against the fallback stack, producing a size-independent
//!    [`TextBlock`] in em units.
//! 2. [`draw`] — rasterize a [`TextBlock`] onto a `tiny_skia::PixmapMut`
//!    at a given origin and font size, with fill color and optional halo.
//!
//! # Scope (phase 1)
//!
//! Deliberately not handled yet; callers should not expect them:
//!
//! - bidi / RTL reordering (input is laid out in logical order, LTR)
//! - vertical writing mode
//! - color-emoji tables (`COLR` / `CBDT`) — such glyphs draw as their
//!   monochrome outline, or not at all if the font has none
//! - MapLibre `format` sections (per-span font / size / color)

pub mod collide;
mod draw;
mod font;
mod layout;
pub mod pbf;
pub mod sdf;
mod shape;

pub use collide::{place, Aabb, Candidate, Grid, COLLISION_CELL_PX, DEDUP_QUANTUM};
pub use draw::{draw, TextPaint};
pub use font::{Font, FontError, StackEntry};
pub use layout::{
    char_allows_ideographic_breaking, layout, Anchor, EmBox, Justify, LayoutParams, PlacedGlyph,
    TextBlock, TextTransform,
};
pub use pbf::{decode_glyph_range, GlyphPbfError, GlyphRange};
pub use sdf::{RangeFetcher, SdfCoverage, SdfFontStack, SdfGlyph};
