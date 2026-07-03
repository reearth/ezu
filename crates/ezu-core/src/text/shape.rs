//! Fallback itemization and shaping.
//!
//! The input string is split into contiguous *runs* by font coverage:
//! for each char, the first stack entry that covers it wins; combining
//! marks stick to the preceding char's entry (when that entry covers
//! them) so a base + mark pair shapes together. Chars no entry covers
//! are dropped and counted, so callers can surface a warning.
//!
//! Each run then shapes per its entry's backend: outline fonts go
//! through rustybuzz (kerning and ligatures apply within the run); SDF
//! glyph stacks map one codepoint to one glyph with the PBF advance —
//! the MapLibre client behaviour, which has no shaping engine.

use super::font::StackEntry;
use super::sdf::{SdfCoverage, SDF_EM_PX};

/// One shaped glyph, in em units.
pub(crate) struct ShapedGlyph {
    /// Index into the font stack the glyph came from.
    pub font: usize,
    /// Outline backend: the font's glyph id. SDF backend: the BMP
    /// codepoint (the glyph protocol's id space).
    pub glyph_id: u16,
    /// Horizontal advance in em, including letter spacing.
    pub x_advance: f32,
    /// Offset from the pen position, in em (y positive up, as shaped).
    pub x_offset: f32,
    pub y_offset: f32,
    /// Index into [`ShapedText::chars`] of the first char of this
    /// glyph's cluster.
    pub char_ix: usize,
}

/// A shaped string: the glyph sequence plus the logical char sequence
/// (post-transform, coverage-filtered) the glyphs' `char_ix` index into.
pub(crate) struct ShapedText {
    pub glyphs: Vec<ShapedGlyph>,
    pub chars: Vec<char>,
    /// Chars covered by no entry in the stack, dropped before shaping.
    pub dropped: usize,
    /// The subset of `dropped` that hit an *unavailable* SDF glyph
    /// range (never loaded and unfetchable, or fetch failed) — distinct
    /// so hosts that must pre-bind ranges get an actionable warning.
    pub missing_range: usize,
}

/// A maximal contiguous span of chars itemized to one stack entry.
struct Run {
    font: usize,
    text: String,
    /// Index into the logical char sequence of the run's first char.
    char_start: usize,
}

/// Itemize `text` over the fallback stack and shape every run.
pub(crate) fn shape(text: &str, fonts: &[StackEntry], letter_spacing_em: f32) -> ShapedText {
    let (runs, chars, dropped, missing_range) = itemize(text, fonts);
    let mut glyphs = Vec::new();
    for run in &runs {
        match &fonts[run.font] {
            StackEntry::Outline(font) => {
                let scale = 1.0 / font.units_per_em();
                // Map a cluster (byte offset into the run's text) back to the
                // logical char index.
                let char_of_byte: Vec<usize> = {
                    let mut map = vec![0usize; run.text.len() + 1];
                    for (char_ix, (byte_ix, _)) in run.text.char_indices().enumerate() {
                        map[byte_ix] = run.char_start + char_ix;
                    }
                    map
                };
                font.with_face(|face| {
                    let mut buffer = rustybuzz::UnicodeBuffer::new();
                    buffer.push_str(&run.text);
                    let shaped = rustybuzz::shape(face, &[], buffer);
                    for (info, pos) in shaped
                        .glyph_infos()
                        .iter()
                        .zip(shaped.glyph_positions().iter())
                    {
                        glyphs.push(ShapedGlyph {
                            font: run.font,
                            glyph_id: info.glyph_id as u16,
                            x_advance: pos.x_advance as f32 * scale + letter_spacing_em,
                            x_offset: pos.x_offset as f32 * scale,
                            y_offset: pos.y_offset as f32 * scale,
                            char_ix: char_of_byte[info.cluster as usize],
                        });
                    }
                });
            }
            StackEntry::Sdf(stack) => {
                // 1 codepoint → 1 glyph; the PBF advance is in px at
                // the 24 px em.
                for (char_ix, c) in run.text.chars().enumerate() {
                    // Coverage was checked during itemization; a miss
                    // here would be a racing range eviction, which the
                    // grow-only range map rules out.
                    let Some(glyph) = stack.glyph(c) else {
                        continue;
                    };
                    glyphs.push(ShapedGlyph {
                        font: run.font,
                        glyph_id: c as u16,
                        x_advance: glyph.advance as f32 / SDF_EM_PX + letter_spacing_em,
                        x_offset: 0.0,
                        y_offset: 0.0,
                        char_ix: run.char_start + char_ix,
                    });
                }
            }
        }
    }
    ShapedText {
        glyphs,
        chars,
        dropped,
        missing_range,
    }
}

/// Split `text` into runs by coverage. Returns the runs, the surviving
/// logical char sequence, the dropped-char count, and how many of the
/// drops were due to unavailable SDF ranges.
fn itemize(text: &str, fonts: &[StackEntry]) -> (Vec<Run>, Vec<char>, usize, usize) {
    let mut runs: Vec<Run> = Vec::new();
    let mut chars: Vec<char> = Vec::new();
    let mut dropped = 0usize;
    let mut missing_range = 0usize;
    let mut prev_font: Option<usize> = None;
    for c in text.chars() {
        let first_covering = fonts.iter().position(|f| f.covers(c));
        // Combining marks stick to the preceding char's font (when it
        // covers them) so base + mark shape as one cluster; otherwise
        // fall back to normal first-covering-font itemization.
        let picked = if is_combining_mark(c) {
            match prev_font {
                Some(p) if fonts[p].covers(c) => Some(p),
                _ => first_covering,
            }
        } else {
            first_covering
        };
        let Some(font) = picked else {
            dropped += 1;
            // Was any SDF entry unable to even consult its range?
            if fonts.iter().any(|f| {
                matches!(f, StackEntry::Sdf(s) if s.coverage(c) == SdfCoverage::RangeUnavailable)
            }) {
                missing_range += 1;
            }
            continue;
        };
        match runs.last_mut() {
            Some(run) if run.font == font => run.text.push(c),
            _ => runs.push(Run {
                font,
                text: c.to_string(),
                char_start: chars.len(),
            }),
        }
        chars.push(c);
        prev_font = Some(font);
    }
    (runs, chars, dropped, missing_range)
}

/// Whether `c` is a combining mark (Unicode combining-diacritic blocks).
fn is_combining_mark(c: char) -> bool {
    matches!(
        c as u32,
        0x0300..=0x036F      // Combining Diacritical Marks
        | 0x1AB0..=0x1AFF    // Combining Diacritical Marks Extended
        | 0x1DC0..=0x1DFF    // Combining Diacritical Marks Supplement
        | 0x20D0..=0x20FF    // Combining Diacritical Marks for Symbols
        | 0xFE20..=0xFE2F // Combining Half Marks
    )
}
