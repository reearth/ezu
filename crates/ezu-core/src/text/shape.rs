//! Fallback itemization and shaping (rustybuzz).
//!
//! The input string is split into contiguous *runs* by font coverage:
//! for each char, the first font in the stack whose cmap covers it
//! wins; combining marks stick to the preceding char's font (when that
//! font covers them) so a base + mark pair shapes together. Chars no
//! font covers are dropped and counted, so callers can surface a
//! warning. Each run is shaped with rustybuzz, which applies kerning
//! and ligatures within the run.

use std::sync::Arc;

use super::font::Font;

/// One shaped glyph, in em units.
pub(crate) struct ShapedGlyph {
    /// Index into the font stack the glyph came from.
    pub font: usize,
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
    /// Chars covered by no font in the stack, dropped before shaping.
    pub dropped: usize,
}

/// A maximal contiguous span of chars itemized to one font.
struct Run {
    font: usize,
    text: String,
    /// Index into the logical char sequence of the run's first char.
    char_start: usize,
}

/// Itemize `text` over the font stack and shape every run.
pub(crate) fn shape(text: &str, fonts: &[Arc<Font>], letter_spacing_em: f32) -> ShapedText {
    let (runs, chars, dropped) = itemize(text, fonts);
    let mut glyphs = Vec::new();
    for run in &runs {
        let font = &fonts[run.font];
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
    ShapedText {
        glyphs,
        chars,
        dropped,
    }
}

/// Split `text` into font runs by coverage. Returns the runs, the
/// surviving logical char sequence, and the dropped-char count.
fn itemize(text: &str, fonts: &[Arc<Font>]) -> (Vec<Run>, Vec<char>, usize) {
    let mut runs: Vec<Run> = Vec::new();
    let mut chars: Vec<char> = Vec::new();
    let mut dropped = 0usize;
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
    (runs, chars, dropped)
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
