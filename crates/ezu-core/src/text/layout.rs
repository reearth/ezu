//! Line breaking, justification, and anchoring — a port of the
//! MapLibre GL JS point-label layout (`shaping.ts`) onto both shaping
//! backends (outline fonts and SDF glyph stacks). Everything here is
//! in em units; the draw step scales by font size.
//!
//! Known divergences from the reference (kept deliberately simple):
//!
//! - Chars covered by no font draw nothing instead of rendering a
//!   missing-glyph box. They still count as chars for line breaking, so a
//!   line of them keeps its slot (contributing no width) exactly as a
//!   line of glyphless chars does in the reference.
//! - With an outline primary font, line metrics (first baseline, block
//!   height) come from its real ascender/descender rather than
//!   MapLibre's fixed 24px-glyph rectangle constants. An SDF primary
//!   uses MapLibre's constants exactly (`line-height × lines` block,
//!   baselines at the fixed −17 px offset) — see [`super::sdf`].
//! - `evaluateBreak`'s width bookkeeping runs over shaped glyph
//!   advances (so ligatures/kerning are measured exactly); break
//!   candidates only exist at cluster boundaries.

use super::bidi;
use super::font::FaceEntry;
use super::sdf::{SDF_EM_PX, SDF_Y_OFFSET_PX};
use super::shape::{shape_sections, ShapeSection, ShapedGlyph, ShapedText};

/// The nine MapLibre text anchors: which part of the block sits on the
/// anchor point (`Left` = the block's left edge touches the point, so
/// the text extends to the right of it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Anchor {
    #[default]
    Center,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Anchor {
    /// Parse the MapLibre kebab-case anchor name.
    pub fn parse(s: &str) -> Option<Anchor> {
        Some(match s {
            "center" => Anchor::Center,
            "left" => Anchor::Left,
            "right" => Anchor::Right,
            "top" => Anchor::Top,
            "bottom" => Anchor::Bottom,
            "top-left" => Anchor::TopLeft,
            "top-right" => Anchor::TopRight,
            "bottom-left" => Anchor::BottomLeft,
            "bottom-right" => Anchor::BottomRight,
            _ => return None,
        })
    }

    /// The fraction of the block's width/height that sits left/above
    /// the anchor point. Also positions a symbol's icon (MapLibre
    /// `icon-anchor` shares this enum).
    pub fn fraction(self) -> (f32, f32) {
        match self {
            Anchor::Center => (0.5, 0.5),
            Anchor::Left => (0.0, 0.5),
            Anchor::Right => (1.0, 0.5),
            Anchor::Top => (0.5, 0.0),
            Anchor::Bottom => (0.5, 1.0),
            Anchor::TopLeft => (0.0, 0.0),
            Anchor::TopRight => (1.0, 0.0),
            Anchor::BottomLeft => (0.0, 1.0),
            Anchor::BottomRight => (1.0, 1.0),
        }
    }
}

/// Line justification within the wrapped block. `Auto` follows the
/// anchor's horizontal side (MapLibre `text-justify: auto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Justify {
    #[default]
    Auto,
    Left,
    Center,
    Right,
}

impl Justify {
    /// Parse the MapLibre justify name.
    pub fn parse(s: &str) -> Option<Justify> {
        Some(match s {
            "auto" => Justify::Auto,
            "left" => Justify::Left,
            "center" => Justify::Center,
            "right" => Justify::Right,
            _ => return None,
        })
    }

    /// The fraction of a line's leftover width shifted to its left,
    /// resolving `Auto` against the anchor.
    fn fraction(self, anchor: Anchor) -> f32 {
        match self {
            Justify::Left => 0.0,
            Justify::Center => 0.5,
            Justify::Right => 1.0,
            Justify::Auto => match anchor {
                Anchor::Left | Anchor::TopLeft | Anchor::BottomLeft => 0.0,
                Anchor::Right | Anchor::TopRight | Anchor::BottomRight => 1.0,
                _ => 0.5,
            },
        }
    }
}

/// MapLibre `text-transform`, applied to the input before shaping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextTransform {
    #[default]
    None,
    Uppercase,
    Lowercase,
}

impl TextTransform {
    /// Parse the MapLibre transform name.
    pub fn parse(s: &str) -> Option<TextTransform> {
        Some(match s {
            "none" => TextTransform::None,
            "uppercase" => TextTransform::Uppercase,
            "lowercase" => TextTransform::Lowercase,
            _ => return None,
        })
    }
}

/// Layout parameters, all in em (scaled by font size at draw time).
#[derive(Debug, Clone, Copy)]
pub struct LayoutParams {
    /// Target wrap width in em (MapLibre `text-max-width`). `0` = no
    /// wrapping.
    pub max_width_em: f32,
    /// Distance between line baselines in em (MapLibre
    /// `text-line-height`, default 1.2).
    pub line_height_em: f32,
    /// Extra advance added to every glyph, in em (MapLibre
    /// `text-letter-spacing`).
    pub letter_spacing_em: f32,
    pub anchor: Anchor,
    pub justify: Justify,
    /// Block shift in em, applied after anchoring (MapLibre
    /// `text-offset`).
    pub offset_em: [f32; 2],
    pub transform: TextTransform,
}

impl Default for LayoutParams {
    fn default() -> Self {
        LayoutParams {
            max_width_em: 10.0,
            line_height_em: 1.2,
            letter_spacing_em: 0.0,
            anchor: Anchor::Center,
            justify: Justify::Auto,
            offset_em: [0.0, 0.0],
            transform: TextTransform::None,
        }
    }
}

/// One positioned glyph of a laid-out block. Coordinates are the
/// glyph's baseline origin in em, relative to the anchor point, y down.
#[derive(Debug, Clone, Copy)]
pub struct PlacedGlyph {
    /// Index into the font stack the block was laid out against.
    pub font: usize,
    /// Outline entries: the font's glyph id. SDF entries: the BMP
    /// codepoint.
    pub glyph_id: u16,
    pub x: f32,
    pub y: f32,
    /// Horizontal advance in em (including letter spacing, already scaled
    /// by `scale`). Point placement ignores it; line placement needs it
    /// to find each glyph's horizontal centre along the path.
    pub advance: f32,
    /// The glyph's `format` section `font-scale` (`1.0` for plain text).
    /// The draw step multiplies the font size by it.
    pub scale: f32,
    /// Index of the `format` section this glyph belongs to (`0` for plain
    /// text); indexes the per-section paint table at draw time.
    pub section: u16,
}

/// MapLibre `format` per-section `vertical-align`: how a section's glyphs sit
/// against a line's baseline when the line mixes `font-scale`s. `Baseline`
/// (the default) keeps every section on the shared baseline; the others align
/// a smaller section's top / centre / bottom to the tallest section's. With a
/// single scale per line every option is a no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerticalAlign {
    #[default]
    Baseline,
    Top,
    Center,
    Bottom,
}

impl VerticalAlign {
    /// Parse the MapLibre `vertical-align` name; unknown → `None`.
    pub fn parse(s: &str) -> Option<VerticalAlign> {
        Some(match s {
            "baseline" => VerticalAlign::Baseline,
            "top" | "text-top" => VerticalAlign::Top,
            "center" => VerticalAlign::Center,
            "bottom" | "text-bottom" => VerticalAlign::Bottom,
            _ => return None,
        })
    }
}

/// One `format` section to lay out: its text, the subrange of `fonts` that is
/// its fallback stack, its MapLibre `font-scale` (`1.0` = none), and its
/// `vertical-align` within a mixed-scale line.
#[derive(Debug, Clone)]
pub struct SectionSpec<'a> {
    pub text: &'a str,
    pub fonts: std::ops::Range<usize>,
    pub scale: f32,
    pub valign: VerticalAlign,
}

/// Axis-aligned box in em, relative to the anchor point (y down).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EmBox {
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

impl EmBox {
    pub fn width(&self) -> f32 {
        self.max_x - self.min_x
    }
    pub fn height(&self) -> f32 {
        self.max_y - self.min_y
    }
}

/// A laid-out label: positioned glyphs plus the block's typographic
/// bounding box (the future collision box), both in em relative to the
/// anchor point.
#[derive(Debug, Default)]
pub struct TextBlock {
    pub glyphs: Vec<PlacedGlyph>,
    pub bbox: EmBox,
    /// Chars covered by no font in the stack, dropped before shaping.
    /// Callers can surface a warning when non-zero.
    pub dropped_chars: usize,
    /// The subset of `dropped_chars` that hit an SDF glyph range that
    /// was unavailable (unloaded with no fetcher, or fetch failed) —
    /// callers can point hosts at pre-binding the missing ranges.
    pub missing_range_chars: usize,
}

impl TextBlock {
    /// Whether the block has anything to draw.
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }
}

/// Shape and lay out `text` against a fallback stack. The first entry
/// is the primary: it sets the line metrics — an outline font through
/// its real ascender/descender, an SDF stack through MapLibre's fixed
/// 24 px-em constants.
pub fn layout(text: &str, fonts: &[FaceEntry<'_>], params: &LayoutParams) -> TextBlock {
    layout_sections(
        &[SectionSpec {
            text,
            fonts: 0..fonts.len(),
            scale: 1.0,
            valign: VerticalAlign::Baseline,
        }],
        fonts,
        params,
    )
}

/// Lay out a sequence of `format` sections against a flat font stack (each
/// section's `fonts` a subrange of it). Line metrics come from the primary
/// entry (`fonts[0]`) as in [`layout`]; a section's `font-scale` is baked into
/// its glyph advances and carried on each [`PlacedGlyph`] for the draw step.
/// `layout(text, …)` is the one-section, unscaled case and lays out
/// bit-identically.
pub fn layout_sections(
    sections: &[SectionSpec<'_>],
    fonts: &[FaceEntry<'_>],
    params: &LayoutParams,
) -> TextBlock {
    let (Some(primary), false) = (fonts.first(), sections.iter().all(|s| s.text.is_empty())) else {
        return TextBlock::default();
    };
    let transformed: Vec<String> = sections
        .iter()
        .map(|s| match params.transform {
            TextTransform::None => s.text.to_string(),
            TextTransform::Uppercase => s.text.to_uppercase(),
            TextTransform::Lowercase => s.text.to_lowercase(),
        })
        .collect();
    let shape_secs: Vec<ShapeSection<'_>> = sections
        .iter()
        .zip(&transformed)
        .map(|(s, t)| ShapeSection {
            text: t,
            fonts: s.fonts.clone(),
            scale: s.scale,
        })
        .collect();
    let shaped = shape_sections(&shape_secs, fonts, params.letter_spacing_em);
    if shaped.glyphs.is_empty() {
        return TextBlock {
            dropped_chars: shaped.dropped,
            missing_range_chars: shaped.missing_range,
            ..TextBlock::default()
        };
    }

    let breaks = determine_line_breaks(&shaped, params.max_width_em);
    let lines = split_lines(&shaped, &breaks);

    let lh = params.line_height_em;
    // Unscaled ascent above / descent below the baseline (em), from the
    // primary entry. An outline font uses its real metrics; an SDF stack uses
    // MapLibre's fixed 24 px-em constants — a line whose baseline sits
    // `0.5·line-height − 17px` from its slot top, so ascent/descent split the
    // `line-height` slot around that point.
    let (base_asc, base_desc) = match primary {
        FaceEntry::Outline { font, .. } => (font.ascent_em(), font.descent_em()),
        FaceEntry::Sdf(_) => {
            let asc = 0.5 * lh + SDF_Y_OFFSET_PX / SDF_EM_PX;
            (asc, lh - asc)
        }
    };

    // Each line's metrics scale by its largest `font-scale` (a bigger glyph
    // needs a taller slot); an empty line keeps scale 1. Baselines accumulate,
    // the gap between two lines scaling by the larger of the pair so neither
    // overlaps. All-`1.0` scales reproduce the fixed `first + i·lh` spacing.
    let line_scale: Vec<f32> = lines
        .iter()
        .map(|l| {
            l.glyphs
                .iter()
                .map(|g| g.scale)
                .reduce(f32::max)
                .unwrap_or(1.0)
        })
        .collect();
    let mut baselines = Vec::with_capacity(lines.len());
    for i in 0..lines.len() {
        let b = if i == 0 {
            base_asc * line_scale[0]
        } else {
            baselines[i - 1] + lh * line_scale[i - 1].max(line_scale[i])
        };
        baselines.push(b);
    }
    let last = lines.len() - 1;
    let block_h = baselines[last] + base_desc * line_scale[last];
    let block_w = lines.iter().map(|l| l.width).fold(0.0f32, f32::max);

    let justify = params.justify.fraction(params.anchor);
    let (ax, ay) = params.anchor.fraction();
    let shift_x = -ax * block_w + params.offset_em[0];
    let shift_y = -ay * block_h + params.offset_em[1];

    let mut glyphs = Vec::new();
    for (line_ix, line) in lines.iter().enumerate() {
        let s_line = line_scale[line_ix];
        let line_x = (block_w - line.width) * justify + shift_x;
        let baseline = baselines[line_ix] + shift_y;
        let mut pen = 0.0f32;
        for g in &line.glyphs {
            // A section smaller than the line's tallest is shifted within the
            // line box per its `vertical-align`; equal scales shift by 0.
            let valign = sections
                .get(g.section as usize)
                .map(|s| s.valign)
                .unwrap_or_default();
            let dy = valign_shift(valign, base_asc, base_desc, s_line, g.scale);
            glyphs.push(PlacedGlyph {
                font: g.font,
                glyph_id: g.glyph_id,
                x: line_x + pen + g.x_offset,
                // Shaping offsets are y-up; block coordinates are y-down.
                y: baseline + dy - g.y_offset,
                advance: g.x_advance,
                scale: g.scale,
                section: g.section,
            });
            pen += g.x_advance;
        }
    }

    TextBlock {
        glyphs,
        bbox: EmBox {
            min_x: shift_x,
            min_y: shift_y,
            max_x: shift_x + block_w,
            max_y: shift_y + block_h,
        },
        dropped_chars: shaped.dropped,
        missing_range_chars: shaped.missing_range,
    }
}

/// Baseline shift (em, y-down) that places a section of scale `s_g` within a
/// line whose tallest section has scale `s_line`, per `vertical-align`. Zero
/// when the scales match, so a single-scale line is never perturbed.
fn valign_shift(v: VerticalAlign, base_asc: f32, base_desc: f32, s_line: f32, s_g: f32) -> f32 {
    let (asc_line, desc_line) = (base_asc * s_line, base_desc * s_line);
    let (asc_g, desc_g) = (base_asc * s_g, base_desc * s_g);
    match v {
        VerticalAlign::Baseline => 0.0,
        // Glyph bottom to the line bottom (down is +).
        VerticalAlign::Bottom => desc_line - desc_g,
        // Glyph top to the line top (up is −).
        VerticalAlign::Top => -(asc_line - asc_g),
        // Glyph vertical centre to the line's.
        VerticalAlign::Center => ((desc_line - desc_g) - (asc_line - asc_g)) / 2.0,
    }
}

/// One wrapped line: its glyphs in visual order (whitespace-trimmed at
/// both ends) and their total advance.
struct Line<'a> {
    glyphs: Vec<&'a ShapedGlyph>,
    width: f32,
}

/// Split the shaped glyphs at the char-index `breaks`, trimming
/// whitespace glyphs at both ends of each line (a break eats the space
/// it happened at, like MapLibre's `TaggedString.trim`) and reordering
/// each line from logical into visual order (UAX #9 rule L2). Trimming
/// precedes reordering so the whitespace dropped is the logical line's,
/// which is what rule L1 asks for.
fn split_lines<'a>(shaped: &'a ShapedText, breaks: &[usize]) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    let mut glyph_start = 0usize;
    for &brk in breaks {
        let glyph_end = shaped.glyphs[glyph_start..]
            .iter()
            .position(|g| g.char_ix >= brk)
            .map(|p| glyph_start + p)
            .unwrap_or(shaped.glyphs.len());
        let mut slice = &shaped.glyphs[glyph_start..glyph_end];
        while let Some(g) = slice.first() {
            if !is_whitespace(shaped.chars[g.char_ix]) {
                break;
            }
            slice = &slice[1..];
        }
        while let Some(g) = slice.last() {
            if !is_whitespace(shaped.chars[g.char_ix]) {
                break;
            }
            slice = &slice[..slice.len() - 1];
        }
        let mut glyphs: Vec<&ShapedGlyph> = slice.iter().collect();
        bidi::reorder_visual(&mut glyphs, |g| g.level);
        lines.push(Line {
            width: glyphs.iter().map(|g| g.x_advance).sum(),
            glyphs,
        });
        glyph_start = glyph_end;
    }
    lines
}

// ---------------------------------------------------------------------------
// Line-break choice — the penalty-based dynamic program of maplibre-gl-js
// `shaping.ts` (`determineLineBreaks` / `evaluateBreak` / `leastBadBreaks`):
// minimize the squared deviation of each line from the target width, with
// penalties around punctuation and a bonus for a short last line.

/// A candidate break in the DP: the logical char index the next line
/// starts at, the accumulated width up to it, the best prior break
/// (index into the candidate list), and the accumulated badness.
struct BreakCandidate {
    char_ix: usize,
    x: f32,
    prior: Option<usize>,
    /// Accumulated badness. `f64` like the reference's numbers: the
    /// mandatory-break penalty is eight orders of magnitude above a typical
    /// raggedness term, which `f32` would round away — and with it the
    /// ordering between two chains that both take a mandatory break.
    badness: f64,
}

/// Choose line breaks for the shaped text. Returns the char index each
/// line ends at (exclusive), always ending with `chars.len()`.
fn determine_line_breaks(shaped: &ShapedText, max_width_em: f32) -> Vec<usize> {
    let end = shaped.chars.len();
    // Target width: the total advance spread over the ideal line count.
    // Whitespace counts here but not in the per-line accumulation below,
    // mirroring the reference (spaces at wraps are trimmed away). A
    // non-positive width means "don't wrap" — MapLibre's unbounded
    // `maxWidth`, which line placement passes — so the target is the whole
    // run and no line is ragged. The search still runs: an explicit `\n`
    // carries a penalty far past any raggedness cost, so a mandatory break
    // splits the label either way.
    let total: f32 = shaped.glyphs.iter().map(|g| g.x_advance).sum();
    let target = if max_width_em > 0.0 {
        total / (total / max_width_em).ceil().max(1.0)
    } else {
        total
    };

    // MapLibre only penalizes ideographic breaks when the text carries
    // explicit server-supplied breaks (zero-width spaces).
    let has_zwsp = shaped.chars.contains(&'\u{200b}');

    // Per-char advance (a cluster's total lands on its first char) and which
    // chars a glyph starts at, so the walk below is over the logical chars
    // like the reference's — a char that shaped to nothing (uncovered by the
    // stack) still breaks the line it sits in.
    let mut advance = vec![0.0f32; end];
    let mut glyph_start = vec![false; end];
    for g in &shaped.glyphs {
        advance[g.char_ix] += g.x_advance;
        glyph_start[g.char_ix] = true;
    }

    let mut candidates: Vec<BreakCandidate> = Vec::new();
    let mut current_x = 0.0f32;
    for (i, (&c, &adv)) in shaped.chars.iter().zip(&advance).enumerate() {
        if !is_whitespace(c) {
            current_x += adv;
        }
        let next_ix = i + 1;
        if next_ix >= end {
            break;
        }
        // A break can only fall on a cluster boundary (never inside a
        // ligature): a char that starts a glyph, or one no glyph covers.
        if !glyph_start[next_ix] && shaped.covered[next_ix] {
            continue;
        }
        let ideographic = char_allows_ideographic_breaking(c);
        if is_breakable(c) || ideographic {
            let penalty =
                calculate_penalty(c, Some(shaped.chars[next_ix]), ideographic && has_zwsp);
            let cand = evaluate_break(next_ix, current_x, target, &candidates, penalty, false);
            candidates.push(cand);
        }
    }
    let last = evaluate_break(end, current_x, target, &candidates, 0.0, true);
    least_bad_breaks(&last, &candidates)
}

/// Badness of a line of `line_width` against the target: squared
/// raggedness plus the (signed-squared) break penalty; a short last
/// line is half-forgiven.
fn calculate_badness(line_width: f32, target: f32, penalty: f32, is_last: bool) -> f64 {
    let raggedness = f64::from(line_width - target).powi(2);
    let penalty = f64::from(penalty);
    if is_last && line_width < target {
        return raggedness / 2.0;
    }
    raggedness + penalty.abs() * penalty
}

/// Penalty for breaking after `c` (the last char of a line) before
/// `next` (the first char of the next line).
fn calculate_penalty(c: char, next: Option<char>, penalizable_ideographic: bool) -> f32 {
    let mut penalty = 0.0f32;
    // A newline forces a break.
    if c == '\n' {
        penalty -= 10000.0;
    }
    // Breaks between ideographic chars are less preferable than breaks
    // at explicit zero-width spaces.
    if penalizable_ideographic {
        penalty += 150.0;
    }
    // Penalize an open parenthesis at the end of a line …
    if c == '(' || c == '\u{ff08}' {
        penalty += 50.0;
    }
    // … and a close parenthesis at the start of one.
    if next == Some(')') || next == Some('\u{ff09}') {
        penalty += 50.0;
    }
    penalty
}

/// Evaluate one candidate break at accumulated width `x`: either start
/// a fresh first line or chain onto whichever prior break minimizes
/// the accumulated badness.
fn evaluate_break(
    char_ix: usize,
    x: f32,
    target: f32,
    candidates: &[BreakCandidate],
    penalty: f32,
    is_last: bool,
) -> BreakCandidate {
    let mut best_prior = None;
    let mut best_badness = calculate_badness(x, target, penalty, is_last);
    for (ix, prior) in candidates.iter().enumerate() {
        let line_width = x - prior.x;
        let badness = calculate_badness(line_width, target, penalty, is_last) + prior.badness;
        if badness <= best_badness {
            best_prior = Some(ix);
            best_badness = badness;
        }
    }
    BreakCandidate {
        char_ix,
        x,
        prior: best_prior,
        badness: best_badness,
    }
}

/// Walk the prior-break chain of the final candidate into an ordered
/// list of break char indices (ending with the text length).
fn least_bad_breaks(last: &BreakCandidate, candidates: &[BreakCandidate]) -> Vec<usize> {
    let mut breaks = vec![last.char_ix];
    let mut prior = last.prior;
    while let Some(ix) = prior {
        breaks.push(candidates[ix].char_ix);
        prior = candidates[ix].prior;
    }
    breaks.reverse();
    breaks
}

/// The whitespace set MapLibre trims at line breaks.
fn is_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ')
}

/// Chars a line may break after even without surrounding spaces
/// (MapLibre's `breakable` map): whitespace plus word-breaking
/// punctuation.
fn is_breakable(c: char) -> bool {
    matches!(
        c,
        '\n' | ' '
            | '&'
            | '('
            | ')'
            | '+'
            | '-'
            | '\u{ad}'   // soft hyphen
            | '\u{b7}'   // middle dot
            | '\u{200b}' // zero-width space
            | '\u{2010}' // hyphen
            | '\u{2013}' // en dash
            | '\u{2027}' // interpunct
            | '/'
    )
}

/// Whether a line may break after `c` without punctuation or spaces —
/// CJK and other scripts that wrap anywhere. Mirrors maplibre-gl-js
/// `charAllowsIdeographicBreaking` (block-range based).
pub fn char_allows_ideographic_breaking(c: char) -> bool {
    let u = c as u32;
    // Everything below the CJK Radicals Supplement is out.
    if u < 0x2e80 {
        return false;
    }
    matches!(
        u,
        0x2e80..=0x2eff      // CJK Radicals Supplement
        | 0x2f00..=0x2fdf    // Kangxi Radicals
        | 0x2ff0..=0x2fff    // Ideographic Description Characters
        | 0x3000..=0x303f    // CJK Symbols and Punctuation
        | 0x3040..=0x309f    // Hiragana
        | 0x30a0..=0x30ff    // Katakana
        | 0x3100..=0x312f    // Bopomofo
        | 0x31a0..=0x31bf    // Bopomofo Extended
        | 0x31c0..=0x31ef    // CJK Strokes
        | 0x31f0..=0x31ff    // Katakana Phonetic Extensions
        | 0x3200..=0x32ff    // Enclosed CJK Letters and Months
        | 0x3300..=0x33ff    // CJK Compatibility
        | 0x3400..=0x4dbf    // CJK Unified Ideographs Extension A
        | 0x4e00..=0x9fff    // CJK Unified Ideographs
        | 0xa000..=0xa48f    // Yi Syllables
        | 0xa490..=0xa4cf    // Yi Radicals
        | 0xf900..=0xfaff    // CJK Compatibility Ideographs
        | 0xfe10..=0xfe1f    // Vertical Forms
        | 0xfe30..=0xfe4f    // CJK Compatibility Forms
        | 0xff00..=0xffef // Halfwidth and Fullwidth Forms
    )
}
