//! Fallback itemization and shaping.
//!
//! The input string is split into contiguous *runs* by font coverage
//! and bidi embedding level: for each char, the first stack entry that
//! covers it wins; combining marks stick to the preceding char's entry
//! (when that entry covers them) so a base + mark pair shapes together.
//! A change of level ends a run too, so a run is always one direction
//! and the shaper can be told which. Chars no entry covers
//! draw nothing and are counted, so callers can surface a warning; they
//! stay in the logical char sequence, which line breaking runs over (a
//! `\n` is a mandatory break whether or not any font has a glyph for
//! it), and they end the run they fall in so a run's text is always
//! contiguous in that sequence.
//!
//! Each run then shapes per its entry's backend: outline fonts go
//! through rustybuzz (kerning and ligatures apply within the run); SDF
//! glyph stacks map one codepoint to one glyph with the PBF advance —
//! the MapLibre client behaviour, which has no shaping engine. What an
//! SDF stack does get is Arabic joining, by asking it for the
//! presentation form the letter's context calls for instead of the
//! letter itself (see [`super::arabic`]).
//!
//! Glyphs come out in **logical** order whichever backend produced
//! them: rustybuzz emits a right-to-left run already reversed, and that
//! is undone here so line breaking and [`super::layout`]'s reordering
//! see one order. Visual order is settled once, per line, by rule L2.

use std::ops::Range;

use super::arabic;
use super::bidi;
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
    /// Bidi embedding level of the glyph's run (even = left-to-right),
    /// which line reordering reverses stretches of.
    pub level: u8,
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

/// A maximal contiguous span of chars itemized to one stack entry and
/// one bidi embedding level.
struct Run {
    font: usize,
    text: String,
    /// Per char of `text`, the codepoint the SDF backend draws it as —
    /// its Arabic presentation form where one applies, `None` where a
    /// neighbour's ligature already covers it. Unused by the outline
    /// backend, which shapes `text` itself.
    draw: Vec<Option<char>>,
    /// Index into the logical char sequence of the run's first char.
    char_start: usize,
    /// Bidi embedding level shared by every char of the run.
    level: u8,
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
    // Bidi resolves over the sections joined, not each on its own: they
    // are one logical string, and a section boundary is not a paragraph
    // break (`format` splits a label by styling, not by direction).
    let joined: String = sections.iter().map(|s| s.text).collect();
    let all_levels = bidi::levels(&joined);
    let mut level_base = 0usize;
    for (sec_ix, sec) in sections.iter().enumerate() {
        let base = sec.fonts.start;
        let sub = &fonts[sec.fonts.clone()];
        let sec_levels = &all_levels[level_base..level_base + sec.text.chars().count()];
        level_base += sec_levels.len();
        let it = itemize(sec.text, sub, sec_levels);
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
    let rtl = run.level % 2 == 1;
    // Letter spacing would open gaps inside a cursive word, breaking the
    // very joins that were drawn for it, so a run holding one goes
    // without. This is not an SDF concern: an outline font joins the run
    // from its own `GSUB`, and tracking pulls those joins apart just the
    // same. MapLibre suppresses over the same idea, though it decides per
    // label rather than per run, so its Latin half loses spacing too.
    let spacing = if run.text.chars().all(arabic::allows_letter_spacing) {
        letter_spacing_em
    } else {
        0.0
    };
    match entry {
        FaceEntry::Outline { font, face } => {
            let units = 1.0 / font.units_per_em();
            let first = glyphs.len();
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
            // Guess first (it fills script and language from the run's
            // chars), then override the direction it derived from the
            // script with the one bidi resolved — they differ for, say,
            // a Latin word quoted inside an Arabic sentence.
            buffer.guess_segment_properties();
            buffer.set_direction(if rtl {
                rustybuzz::Direction::RightToLeft
            } else {
                rustybuzz::Direction::LeftToRight
            });
            let shaped = rustybuzz::shape(face, &[], buffer);
            for (info, pos) in shaped
                .glyph_infos()
                .iter()
                .zip(shaped.glyph_positions().iter())
            {
                glyphs.push(ShapedGlyph {
                    font: font_ix,
                    glyph_id: info.glyph_id as u16,
                    x_advance: pos.x_advance as f32 * units * scale + spacing,
                    x_offset: pos.x_offset as f32 * units * scale,
                    y_offset: pos.y_offset as f32 * units * scale,
                    char_ix: char_of_byte[info.cluster as usize],
                    scale,
                    section,
                    level: run.level,
                });
            }
            // rustybuzz hands back a right-to-left run in visual order;
            // put it back in logical order so everything downstream sees
            // one convention and rule L2 reverses it exactly once.
            if rtl {
                glyphs[first..].reverse();
            }
        }
        FaceEntry::Sdf(stack) => {
            // 1 codepoint → 1 glyph; the PBF advance is in px at the 24 px em.
            for (char_ix, draw) in run.draw.iter().enumerate() {
                // A char a neighbour's ligature already drew.
                let Some(c) = *draw else {
                    continue;
                };
                // Coverage was checked during itemization; a miss here would be
                // a racing range eviction, which the grow-only range map rules
                // out.
                let Some(glyph) = stack.glyph(c) else {
                    continue;
                };
                glyphs.push(ShapedGlyph {
                    font: font_ix,
                    glyph_id: c as u16,
                    x_advance: glyph.advance as f32 / SDF_EM_PX * scale + spacing,
                    x_offset: 0.0,
                    y_offset: 0.0,
                    char_ix: char_base + run.char_start + char_ix,
                    scale,
                    section,
                    level: run.level,
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

/// What one char resolved to: the stack entry that will draw it, and the
/// codepoint that entry will be asked for — `None` for a char another
/// glyph already covers (the alef of a lam-alef ligature).
type Resolved = Option<(usize, Option<char>)>;

/// Split `text` into runs by coverage and bidi level (`levels` per char of
/// `text`). Uncovered chars shape to nothing but stay in `chars` (line
/// breaking needs them) and end the open run, so every run's text is
/// contiguous in the char sequence.
fn itemize(text: &str, fonts: &[FaceEntry<'_>], levels: &[u8]) -> Itemized {
    let all: Vec<char> = text.chars().collect();
    let resolved = resolve(&all, fonts);

    let mut runs: Vec<Run> = Vec::new();
    let mut chars: Vec<char> = Vec::new();
    let mut covered: Vec<bool> = Vec::new();
    let mut dropped = 0usize;
    let mut missing_range = 0usize;
    // Logical index just past the open run's last char; a gap means an
    // uncovered char intervened and the run must not be extended.
    let mut run_end = 0usize;
    for (ix, &c) in all.iter().enumerate() {
        let Some((font, draw)) = resolved[ix] else {
            dropped += 1;
            // Was any SDF entry unable to even consult its range?
            if fonts.iter().any(|f| {
                matches!(f, FaceEntry::Sdf(s) if s.coverage(c) == SdfCoverage::RangeUnavailable)
            }) {
                missing_range += 1;
            }
            chars.push(c);
            covered.push(false);
            continue;
        };
        let level = levels[ix];
        match runs.last_mut() {
            Some(run) if run.font == font && run.level == level && run_end == chars.len() => {
                run.text.push(c);
                run.draw.push(draw);
            }
            _ => runs.push(Run {
                font,
                text: c.to_string(),
                draw: vec![draw],
                char_start: chars.len(),
                level,
            }),
        }
        chars.push(c);
        covered.push(true);
        run_end = chars.len();
    }
    Itemized {
        runs,
        chars,
        covered,
        dropped,
        missing_range,
    }
}

/// Resolve every char of `all` against the stack: which entry draws it,
/// and as which codepoint.
///
/// An outline entry is asked for the char itself — it has the tables to
/// shape it. An SDF entry, which has neither tables nor outlines, is
/// asked for the Arabic presentation form the char's joining context
/// calls for, and only falls back to the char itself when the stack does
/// not carry that form; this is where a glyph stack gets joined Arabic.
fn resolve(all: &[char], fonts: &[FaceEntry<'_>]) -> Vec<Resolved> {
    let mut out: Vec<Resolved> = vec![None; all.len()];
    let mut prev_font: Option<usize> = None;
    // Set on the alef a preceding lam ligated with: the pair draws as
    // one glyph, so the alef contributes none of its own.
    let mut ligated = vec![false; all.len()];
    // The codepoints to try for the char in hand, most specific first;
    // reused across chars so a label of plain Latin allocates nothing.
    let mut wanted: Vec<char> = Vec::new();
    for ix in 0..all.len() {
        let c = all[ix];
        if ligated[ix] {
            // Stays on the lam's entry so the run does not break at it.
            out[ix] = prev_font.map(|f| (f, None));
            continue;
        }
        let (from_previous, to_next) = joining_context(all, ix);
        wanted.clear();
        let ligature = if arabic::is_lam(c) {
            next_joinable(all, ix).and_then(|n| arabic::lam_alef(all[n], from_previous))
        } else {
            None
        };
        wanted.extend(ligature);
        arabic::shaped_forms(c, from_previous, to_next, &mut wanted);
        wanted.push(c);

        // Marks stick to the preceding char's entry (when it covers
        // them) so base + mark shape as one cluster; otherwise fall
        // back to the first entry in the stack that covers the char.
        let mark_font = if is_mark(c) { prev_font } else { None };
        let order = mark_font.into_iter().chain(0..fonts.len());
        let picked = order
            .filter_map(|i| draws(&fonts[i], c, &wanted).map(|cp| (i, cp)))
            .next();

        let Some((font, cp)) = picked else {
            prev_font = None;
            continue;
        };
        if Some(cp) == ligature {
            if let Some(n) = next_joinable(all, ix) {
                ligated[n] = true;
            }
        }
        out[ix] = Some((font, Some(cp)));
        prev_font = Some(font);
    }
    out
}

/// The codepoint `entry` would draw `c` as, of the `wanted` shapes
/// (most specific first, the bare char last), or `None` if it covers
/// none of them.
fn draws(entry: &FaceEntry<'_>, c: char, wanted: &[char]) -> Option<char> {
    match entry {
        // Its own `GSUB` does the joining, from the char itself.
        FaceEntry::Outline { .. } => entry.covers(c).then_some(c),
        FaceEntry::Sdf(_) => wanted.iter().copied().find(|&cp| entry.covers(cp)),
    }
}

/// Whether the char before `all[ix]` joins to it and whether the char
/// after does, looking past transparent marks on both sides.
fn joining_context(all: &[char], ix: usize) -> (bool, bool) {
    let previous = all[..ix]
        .iter()
        .rev()
        .find(|c| !arabic::is_transparent(**c))
        .copied();
    let next = next_joinable(all, ix).map(|n| all[n]);
    arabic::joined_sides(previous, all[ix], next)
}

/// The index of the first char after `ix` that joining looks at.
fn next_joinable(all: &[char], ix: usize) -> Option<usize> {
    all.iter()
        .enumerate()
        .skip(ix + 1)
        .find(|(_, c)| !arabic::is_transparent(**c))
        .map(|(i, _)| i)
}

/// Whether `c` is a mark that belongs on the preceding char's entry —
/// the combining-diacritic blocks, plus the Arabic marks joining looks
/// past.
fn is_mark(c: char) -> bool {
    arabic::is_transparent(c) || is_combining_mark(c)
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
