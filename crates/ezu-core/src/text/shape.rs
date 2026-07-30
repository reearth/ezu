//! Fallback itemization and shaping.
//!
//! The input string is split into contiguous *runs* by font coverage:
//! for each char, the first stack entry that covers it wins; combining
//! marks stick to the preceding char's entry (when that entry covers
//! them) so a base + mark pair shapes together. Chars no entry covers
//! draw nothing and are counted, so callers can surface a warning; they
//! stay in the logical char sequence, which line breaking runs over (a
//! `\n` is a mandatory break whether or not any font has a glyph for
//! it), and they end the run they fall in so a run's text is always
//! contiguous in that sequence.
//!
//! Each run then shapes per its entry's backend: outline fonts go
//! through rustybuzz (kerning and ligatures apply within the run); SDF
//! glyph stacks map one codepoint to one glyph with the PBF advance —
//! the MapLibre client behaviour, which has no shaping engine.

use std::ops::Range;

use super::font::FaceEntry;
use super::sdf::{SdfCoverage, SDF_EM_PX};

/// One shaped glyph, in em units.
pub(crate) struct ShapedGlyph {
    /// Index into the font stack the glyph came from.
    pub font: usize,
    /// Outline backend: the font's glyph id. SDF backend: the BMP
    /// codepoint (the glyph protocol's id space).
    pub glyph_id: u16,
    /// Horizontal advance in em, including letter spacing. Already
    /// multiplied by the section's `scale`.
    pub x_advance: f32,
    /// Offset from the pen position, in em (y positive up, as shaped).
    /// Already multiplied by the section's `scale`.
    pub x_offset: f32,
    pub y_offset: f32,
    /// Index into [`ShapedText::chars`] of the first char of this
    /// glyph's cluster.
    pub char_ix: usize,
    /// MapLibre `format` per-section `font-scale` baked into this glyph
    /// (`1.0` for plain text). The draw step multiplies the font size by
    /// it; layout uses it for per-line metrics.
    pub scale: f32,
    /// Index of the `format` section this glyph belongs to (`0` for plain
    /// text), used to look up the per-section paint at draw time.
    pub section: u16,
}

/// One `format` section to shape: its text, the subrange of the flat font
/// stack that is its fallback chain, and its `font-scale`.
pub(crate) struct ShapeSection<'a> {
    pub text: &'a str,
    pub fonts: Range<usize>,
    pub scale: f32,
}

/// A shaped string: the glyph sequence plus the logical char sequence
/// (post-transform, every input char) the glyphs' `char_ix` index into.
pub(crate) struct ShapedText {
    pub glyphs: Vec<ShapedGlyph>,
    pub chars: Vec<char>,
    /// Per char of `chars`: whether some stack entry covered it and it
    /// therefore went through shaping. A `false` entry produced no glyph,
    /// so a line may always break at it.
    pub covered: Vec<bool>,
    /// Chars covered by no entry in the stack, which shape to nothing.
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

/// Shape a sequence of `format` sections against a flat font stack. Each
/// section itemizes over its own subrange (its fallback chain) and its glyphs
/// carry the section index and `font-scale`; a section boundary always ends a
/// run (no cross-section shaping — MapLibre behaves the same). The logical
/// `chars` sequence is the sections concatenated, which the line-break DP and
/// whitespace trimming index into unchanged.
pub(crate) fn shape_sections(
    sections: &[ShapeSection<'_>],
    fonts: &[FaceEntry<'_>],
    letter_spacing_em: f32,
) -> ShapedText {
    let mut glyphs = Vec::new();
    let mut chars: Vec<char> = Vec::new();
    let mut covered: Vec<bool> = Vec::new();
    let mut dropped = 0usize;
    let mut missing_range = 0usize;
    for (sec_ix, sec) in sections.iter().enumerate() {
        let base = sec.fonts.start;
        let sub = &fonts[sec.fonts.clone()];
        let it = itemize(sec.text, sub);
        let char_base = chars.len();
        chars.extend(it.chars);
        covered.extend(it.covered);
        dropped += it.dropped;
        missing_range += it.missing_range;
        let runs = it.runs;
        for run in &runs {
            shape_run(
                run,
                &fonts[base + run.font],
                base,
                sec.scale,
                sec_ix as u16,
                char_base,
                letter_spacing_em,
                &mut glyphs,
            );
        }
    }
    ShapedText {
        glyphs,
        chars,
        covered,
        dropped,
        missing_range,
    }
}

/// Shape one itemized run against `entry` (the stack entry at absolute index
/// `base + run.font`) and append its glyphs. `char_base` maps the run's
/// section-local char indices back into the combined `chars` sequence.
#[allow(clippy::too_many_arguments)]
fn shape_run(
    run: &Run,
    entry: &FaceEntry<'_>,
    base: usize,
    scale: f32,
    section: u16,
    char_base: usize,
    letter_spacing_em: f32,
    glyphs: &mut Vec<ShapedGlyph>,
) {
    let font_ix = base + run.font;
    match entry {
        FaceEntry::Outline { font, face } => {
            let units = 1.0 / font.units_per_em();
            // Map a cluster (byte offset into the run's text) back to the
            // logical char index.
            let char_of_byte: Vec<usize> = {
                let mut map = vec![0usize; run.text.len() + 1];
                for (char_ix, (byte_ix, _)) in run.text.char_indices().enumerate() {
                    map[byte_ix] = char_base + run.char_start + char_ix;
                }
                map
            };
            let mut buffer = rustybuzz::UnicodeBuffer::new();
            buffer.push_str(&run.text);
            let shaped = rustybuzz::shape(face, &[], buffer);
            for (info, pos) in shaped
                .glyph_infos()
                .iter()
                .zip(shaped.glyph_positions().iter())
            {
                glyphs.push(ShapedGlyph {
                    font: font_ix,
                    glyph_id: info.glyph_id as u16,
                    x_advance: pos.x_advance as f32 * units * scale + letter_spacing_em,
                    x_offset: pos.x_offset as f32 * units * scale,
                    y_offset: pos.y_offset as f32 * units * scale,
                    char_ix: char_of_byte[info.cluster as usize],
                    scale,
                    section,
                });
            }
        }
        FaceEntry::Sdf(stack) => {
            // 1 codepoint → 1 glyph; the PBF advance is in px at the 24 px em.
            for (char_ix, c) in run.text.chars().enumerate() {
                // Coverage was checked during itemization; a miss here would be
                // a racing range eviction, which the grow-only range map rules
                // out.
                let Some(glyph) = stack.glyph(c) else {
                    continue;
                };
                glyphs.push(ShapedGlyph {
                    font: font_ix,
                    glyph_id: c as u16,
                    x_advance: glyph.advance as f32 / SDF_EM_PX * scale + letter_spacing_em,
                    x_offset: 0.0,
                    y_offset: 0.0,
                    char_ix: char_base + run.char_start + char_ix,
                    scale,
                    section,
                });
            }
        }
    }
}

/// One itemized section: its runs, its full logical char sequence, the
/// per-char coverage flags, the dropped-char count, and how many of the
/// drops were due to unavailable SDF ranges.
struct Itemized {
    runs: Vec<Run>,
    chars: Vec<char>,
    covered: Vec<bool>,
    dropped: usize,
    missing_range: usize,
}

/// Split `text` into runs by coverage. Uncovered chars shape to nothing but
/// stay in `chars` (line breaking needs them) and end the open run, so every
/// run's text is contiguous in the char sequence.
fn itemize(text: &str, fonts: &[FaceEntry<'_>]) -> Itemized {
    let mut runs: Vec<Run> = Vec::new();
    let mut chars: Vec<char> = Vec::new();
    let mut covered: Vec<bool> = Vec::new();
    let mut dropped = 0usize;
    let mut missing_range = 0usize;
    let mut prev_font: Option<usize> = None;
    // Logical index just past the open run's last char; a gap means an
    // uncovered char intervened and the run must not be extended.
    let mut run_end = 0usize;
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
                matches!(f, FaceEntry::Sdf(s) if s.coverage(c) == SdfCoverage::RangeUnavailable)
            }) {
                missing_range += 1;
            }
            chars.push(c);
            covered.push(false);
            prev_font = None;
            continue;
        };
        match runs.last_mut() {
            Some(run) if run.font == font && run_end == chars.len() => run.text.push(c),
            _ => runs.push(Run {
                font,
                text: c.to_string(),
                char_start: chars.len(),
            }),
        }
        chars.push(c);
        covered.push(true);
        run_end = chars.len();
        prev_font = Some(font);
    }
    Itemized {
        runs,
        chars,
        covered,
        dropped,
        missing_range,
    }
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
