//! Shaping / layout / draw tests for the `text` module, against two
//! vendored Noto Sans subsets with disjoint coverage (letters vs
//! digits) — see `tests/fonts/README.md`.

#![cfg(feature = "text")]

use std::sync::Arc;

use ezu_core::text::{
    char_allows_ideographic_breaking, draw, layout, Anchor, FaceEntry, Font, LayoutParams,
    StackEntry, TextBlock, TextPaint, TextTransform,
};

const LATIN: &[u8] = include_bytes!("fonts/NotoSans-Regular.latin.ttf");
const DIGITS: &[u8] = include_bytes!("fonts/NotoSans-Regular.digits.ttf");

fn latin_font() -> Arc<Font> {
    Arc::new(Font::from_bytes(Arc::from(LATIN), 0).expect("latin subset parses"))
}

fn latin() -> StackEntry {
    StackEntry::Outline(latin_font())
}

fn digits() -> StackEntry {
    StackEntry::Outline(Arc::new(
        Font::from_bytes(Arc::from(DIGITS), 0).expect("digits subset parses"),
    ))
}

fn no_wrap() -> LayoutParams {
    LayoutParams {
        max_width_em: 0.0,
        ..LayoutParams::default()
    }
}

fn layout_one(text: &str, params: &LayoutParams) -> TextBlock {
    let fonts = [latin(), digits()];
    let fonts = FaceEntry::prepare(&fonts);
    layout(text, &fonts, params)
}

// --- itemization / fallback -------------------------------------------------

#[test]
fn itemization_picks_the_first_covering_font() {
    // "A1B": letters live in the latin subset (font 0), the digit only
    // in the digits subset (font 1).
    let block = layout_one("A1B", &no_wrap());
    let fonts: Vec<usize> = block.glyphs.iter().map(|g| g.font).collect();
    assert_eq!(fonts, [0, 1, 0]);
    assert_eq!(block.dropped_chars, 0);
}

#[test]
fn combining_mark_sticks_to_the_preceding_font() {
    // U+0301 (combining acute) is covered by *both* subsets; after a
    // digit it must stay with the digits font rather than falling to
    // the first covering font in the stack.
    let block = layout_one("1\u{0301}", &no_wrap());
    assert!(!block.is_empty());
    assert!(
        block.glyphs.iter().all(|g| g.font == 1),
        "mark should shape with the digit's font: {:?}",
        block.glyphs.iter().map(|g| g.font).collect::<Vec<_>>()
    );
}

#[test]
fn uncovered_chars_are_dropped_and_counted() {
    let block = layout_one("Aあ", &no_wrap());
    assert_eq!(block.glyphs.len(), 1);
    assert_eq!(block.dropped_chars, 1);
}

// --- shaping ---------------------------------------------------------------

#[test]
fn shaping_advances_monotonically() {
    let block = layout_one("Hello", &no_wrap());
    assert_eq!(block.glyphs.len(), 5);
    for w in block.glyphs.windows(2) {
        assert!(w[1].x > w[0].x, "glyph positions must advance: {w:?}");
    }
}

#[test]
fn kerning_tightens_known_pairs() {
    // Noto Sans kerns "AV"; the pair must measure narrower than the
    // two glyphs laid side by side.
    let av = layout_one("AV", &no_wrap()).bbox.width();
    let a = layout_one("A", &no_wrap()).bbox.width();
    let v = layout_one("V", &no_wrap()).bbox.width();
    assert!(
        av < a + v - 1e-4,
        "expected kerning: AV = {av}, A + V = {}",
        a + v
    );
}

#[test]
fn transform_uppercases_before_shaping() {
    let upper = layout_one(
        "abc",
        &LayoutParams {
            transform: TextTransform::Uppercase,
            ..no_wrap()
        },
    );
    let reference = layout_one("ABC", &no_wrap());
    let ids = |b: &TextBlock| b.glyphs.iter().map(|g| g.glyph_id).collect::<Vec<_>>();
    assert_eq!(ids(&upper), ids(&reference));
}

// --- line breaking ----------------------------------------------------------

/// Approximate line count from the block height: 1 line spans
/// ascent+descent (~1.36 em for Noto Sans), each extra line adds
/// `line-height` (1.2 em).
fn line_count(block: &TextBlock, params: &LayoutParams) -> usize {
    let single = latin_font().ascent_em() + latin_font().descent_em();
    (((block.bbox.height() - single) / params.line_height_em).round() as usize) + 1
}

#[test]
fn wrapping_splits_at_spaces_near_max_width() {
    let params = LayoutParams {
        max_width_em: 4.0,
        ..LayoutParams::default()
    };
    let block = layout_one("one two three four", &params);
    let lines = line_count(&block, &params);
    assert!(lines >= 2, "expected a wrap, got {lines} line(s)");
    // No line (hence the block) should be much wider than the target.
    assert!(
        block.bbox.width() < 6.0,
        "lines should stay near max-width: {}",
        block.bbox.width()
    );
}

#[test]
fn a_single_word_never_breaks() {
    let params = LayoutParams {
        max_width_em: 1.0,
        ..LayoutParams::default()
    };
    let block = layout_one("Antidisestablishmentarianism", &params);
    assert_eq!(line_count(&block, &params), 1);
    assert!(block.bbox.width() > params.max_width_em);
}

#[test]
fn zero_max_width_disables_wrapping() {
    let block = layout_one("one two three four five six", &no_wrap());
    assert_eq!(line_count(&block, &no_wrap()), 1);
}

#[test]
fn ideographic_chars_are_break_candidates() {
    // The vendored subsets carry no CJK glyphs, so exercise the
    // break-candidate classification directly (maplibre-gl-js
    // `charAllowsIdeographicBreaking` ranges).
    for c in ['海', 'あ', 'ア', '。', '１', '中'] {
        assert!(char_allows_ideographic_breaking(c), "{c} should break");
    }
    for c in ['A', 'z', '9', ' ', 'ä', 'Я'] {
        assert!(!char_allows_ideographic_breaking(c), "{c} should not");
    }
}

// --- anchoring ---------------------------------------------------------------

#[test]
fn anchors_place_the_block_on_the_expected_side() {
    // (anchor, expects) where expects checks the bbox against the
    // origin: negative = block extends left/up, positive = right/down.
    type BoxCheck = fn(f32, f32, f32, f32) -> bool;
    let cases: [(Anchor, BoxCheck); 9] = [
        (Anchor::Center, |x0, y0, x1, y1| {
            x0 < 0.0 && x1 > 0.0 && y0 < 0.0 && y1 > 0.0
        }),
        (Anchor::Left, |x0, y0, x1, y1| {
            x0.abs() < 1e-4 && x1 > 0.0 && y0 < 0.0 && y1 > 0.0
        }),
        (Anchor::Right, |x0, y0, x1, y1| {
            x0 < 0.0 && x1.abs() < 1e-4 && y0 < 0.0 && y1 > 0.0
        }),
        (Anchor::Top, |x0, y0, x1, y1| {
            x0 < 0.0 && x1 > 0.0 && y0.abs() < 1e-4 && y1 > 0.0
        }),
        (Anchor::Bottom, |x0, y0, x1, y1| {
            x0 < 0.0 && x1 > 0.0 && y0 < 0.0 && y1.abs() < 1e-4
        }),
        (Anchor::TopLeft, |x0, y0, x1, y1| {
            x0.abs() < 1e-4 && x1 > 0.0 && y0.abs() < 1e-4 && y1 > 0.0
        }),
        (Anchor::TopRight, |x0, y0, x1, y1| {
            x0 < 0.0 && x1.abs() < 1e-4 && y0.abs() < 1e-4 && y1 > 0.0
        }),
        (Anchor::BottomLeft, |x0, y0, x1, y1| {
            x0.abs() < 1e-4 && x1 > 0.0 && y0 < 0.0 && y1.abs() < 1e-4
        }),
        (Anchor::BottomRight, |x0, y0, x1, y1| {
            x0 < 0.0 && x1.abs() < 1e-4 && y0 < 0.0 && y1.abs() < 1e-4
        }),
    ];
    for (anchor, ok) in cases {
        let block = layout_one(
            "Hi",
            &LayoutParams {
                anchor,
                ..no_wrap()
            },
        );
        let b = block.bbox;
        assert!(
            ok(b.min_x, b.min_y, b.max_x, b.max_y),
            "anchor {anchor:?} misplaced the block: {b:?}"
        );
    }
}

#[test]
fn offset_shifts_the_block() {
    let base = layout_one("Hi", &no_wrap()).bbox;
    let moved = layout_one(
        "Hi",
        &LayoutParams {
            offset_em: [2.0, -1.0],
            ..no_wrap()
        },
    )
    .bbox;
    assert!((moved.min_x - base.min_x - 2.0).abs() < 1e-4);
    assert!((moved.min_y - base.min_y + 1.0).abs() < 1e-4);
}

// --- drawing ----------------------------------------------------------------

fn render(paint: &TextPaint) -> tiny_skia::Pixmap {
    let fonts = [latin(), digits()];
    let fonts = FaceEntry::prepare(&fonts);
    let block = layout("Ag", &fonts, &no_wrap());
    let mut pixmap = tiny_skia::Pixmap::new(96, 64).unwrap();
    draw(
        &block,
        &fonts,
        &mut pixmap.as_mut(),
        (48.0, 32.0),
        paint,
        &[],
    );
    pixmap
}

#[test]
fn draw_produces_pixels_with_the_fill_color() {
    let pixmap = render(&TextPaint {
        size_px: 32.0,
        color: [1.0, 0.0, 0.0, 1.0],
        halo_color: [1.0, 1.0, 1.0, 1.0],
        halo_width_px: 0.0,
        halo_blur_px: 0.0,
    });
    let red = pixmap
        .pixels()
        .iter()
        .filter(|p| p.alpha() == 255 && p.red() > 200 && p.green() < 50)
        .count();
    assert!(red > 20, "expected solid red text pixels, got {red}");
}

#[test]
fn halo_sits_behind_the_fill() {
    let fill_only = render(&TextPaint {
        size_px: 32.0,
        color: [1.0, 0.0, 0.0, 1.0],
        halo_color: [1.0, 1.0, 1.0, 1.0],
        halo_width_px: 0.0,
        halo_blur_px: 0.0,
    });
    let with_halo = render(&TextPaint {
        size_px: 32.0,
        color: [1.0, 0.0, 0.0, 1.0],
        halo_color: [1.0, 1.0, 1.0, 1.0],
        halo_width_px: 2.0,
        halo_blur_px: 0.0,
    });
    // Every solidly-filled pixel must be unchanged by the halo (the
    // halo never paints over fill) …
    for (a, b) in fill_only.pixels().iter().zip(with_halo.pixels()) {
        if a.alpha() == 255 && a.red() > 200 && a.green() < 50 {
            assert_eq!(a, b, "halo overpainted a fill pixel");
        }
    }
    // … and the halo adds white coverage around the glyphs.
    let white = with_halo
        .pixels()
        .iter()
        .filter(|p| p.alpha() == 255 && p.green() > 200 && p.blue() > 200)
        .count();
    assert!(white > 20, "expected white halo pixels, got {white}");
}

// --- format sections (per-section font / scale) -----------------------------

#[test]
fn single_section_equals_plain_layout() {
    use ezu_core::text::{layout_sections, SectionSpec, VerticalAlign};
    let fonts = [latin(), digits()];
    let fonts = FaceEntry::prepare(&fonts);
    let params = no_wrap();
    let plain = layout("A1B", &fonts, &params);
    let sectioned = layout_sections(
        &[SectionSpec {
            text: "A1B",
            fonts: 0..fonts.len(),
            scale: 1.0,
            valign: VerticalAlign::Baseline,
        }],
        &fonts,
        &params,
    );
    assert_eq!(plain.glyphs.len(), sectioned.glyphs.len());
    for (a, b) in plain.glyphs.iter().zip(&sectioned.glyphs) {
        assert_eq!((a.font, a.glyph_id), (b.font, b.glyph_id));
        assert_eq!(
            (a.x.to_bits(), a.y.to_bits()),
            (b.x.to_bits(), b.y.to_bits())
        );
        assert_eq!(b.scale, 1.0);
        assert_eq!(b.section, 0);
    }
    assert_eq!(plain.bbox, sectioned.bbox);
}

#[test]
fn each_section_shapes_against_its_own_font_subrange() {
    use ezu_core::text::{layout_sections, SectionSpec, VerticalAlign};
    // Flat stack [latin, digits]; section 0 ("AB") may use only latin,
    // section 1 ("12") only digits.
    let fonts = [latin(), digits()];
    let fonts = FaceEntry::prepare(&fonts);
    let block = layout_sections(
        &[
            SectionSpec {
                text: "AB",
                fonts: 0..1,
                scale: 1.0,
                valign: VerticalAlign::Baseline,
            },
            SectionSpec {
                text: "12",
                fonts: 1..2,
                scale: 1.0,
                valign: VerticalAlign::Baseline,
            },
        ],
        &fonts,
        &no_wrap(),
    );
    assert_eq!(block.dropped_chars, 0);
    // Section 0 glyphs shaped by the latin font (index 0), section 1 by
    // the digits font (index 1), and each glyph tagged with its section.
    for g in &block.glyphs {
        match g.section {
            0 => assert_eq!(g.font, 0, "AB should use latin"),
            1 => assert_eq!(g.font, 1, "12 should use digits"),
            s => panic!("unexpected section {s}"),
        }
    }
    assert!(block.glyphs.iter().any(|g| g.section == 0));
    assert!(block.glyphs.iter().any(|g| g.section == 1));
}

#[test]
fn font_scale_grows_a_sections_advances() {
    use ezu_core::text::{layout_sections, SectionSpec, VerticalAlign};
    let fonts = [latin(), digits()];
    let fonts = FaceEntry::prepare(&fonts);
    let width = |scale: f32| {
        layout_sections(
            &[SectionSpec {
                text: "AB",
                fonts: 0..1,
                scale,
                valign: VerticalAlign::Baseline,
            }],
            &fonts,
            &no_wrap(),
        )
        .bbox
        .width()
    };
    // A 2× section is about twice as wide (advances carry the scale).
    let (w1, w2) = (width(1.0), width(2.0));
    assert!(
        (w2 - 2.0 * w1).abs() < 0.05 * w1,
        "2x section width {w2} should be ~2x the 1x width {w1}"
    );
}

#[test]
fn mixed_scale_line_grows_by_the_max_section_scale() {
    use ezu_core::text::{layout_sections, SectionSpec, VerticalAlign};
    // Two sections on one line; the second is 2×. The block must be taller
    // than an all-1× line (the tall section enlarges the line's metrics),
    // and matches a uniformly-2× line's height.
    let fonts = [latin()];
    let fonts = FaceEntry::prepare(&fonts);
    let sec = |text, scale| SectionSpec {
        text,
        fonts: 0..1,
        scale,
        valign: VerticalAlign::Baseline,
    };
    let uniform1 = layout_sections(&[sec("AB", 1.0)], &fonts, &no_wrap());
    let mixed = layout_sections(&[sec("A", 1.0), sec("B", 2.0)], &fonts, &no_wrap());
    let uniform2 = layout_sections(&[sec("AB", 2.0)], &fonts, &no_wrap());
    assert!(
        mixed.bbox.height() > uniform1.bbox.height() + 1e-3,
        "a 2x section should raise the line height: mixed={} 1x={}",
        mixed.bbox.height(),
        uniform1.bbox.height()
    );
    assert!(
        (mixed.bbox.height() - uniform2.bbox.height()).abs() < 1e-3,
        "line height should follow the max scale: mixed={} 2x={}",
        mixed.bbox.height(),
        uniform2.bbox.height()
    );
}

#[test]
fn vertical_align_shifts_a_smaller_section_within_the_line() {
    use ezu_core::text::{layout_sections, SectionSpec, VerticalAlign};
    // A big 2× section and a small 1× section on one line. The small
    // section's glyph baseline sits lower for `Bottom` than for `Top`;
    // `Baseline` keeps it on the shared baseline (between the two).
    let fonts = [latin()];
    let fonts = FaceEntry::prepare(&fonts);
    let block = |valign| {
        layout_sections(
            &[
                SectionSpec {
                    text: "A",
                    fonts: 0..1,
                    scale: 2.0,
                    valign: VerticalAlign::Baseline,
                },
                SectionSpec {
                    text: "b",
                    fonts: 0..1,
                    scale: 1.0,
                    valign,
                },
            ],
            &fonts,
            &no_wrap(),
        )
    };
    // The small section is the second glyph (section 1).
    let small_y =
        |b: &ezu_core::text::TextBlock| b.glyphs.iter().find(|g| g.section == 1).unwrap().y;
    let top = small_y(&block(VerticalAlign::Top));
    let base = small_y(&block(VerticalAlign::Baseline));
    let bottom = small_y(&block(VerticalAlign::Bottom));
    // y is down: top-aligned sits highest (smallest y), bottom lowest.
    assert!(
        top < base - 1e-3,
        "top should sit above baseline: top={top} base={base}"
    );
    assert!(
        bottom > base + 1e-3,
        "bottom should sit below baseline: bottom={bottom} base={base}"
    );
}

#[test]
fn vertical_align_is_a_noop_for_a_single_scale_line() {
    use ezu_core::text::{layout_sections, SectionSpec, VerticalAlign};
    // All sections share scale 1.0, so every vertical-align lays out
    // identically to Baseline.
    let fonts = [latin()];
    let fonts = FaceEntry::prepare(&fonts);
    let block = |valign| {
        layout_sections(
            &[
                SectionSpec {
                    text: "A",
                    fonts: 0..1,
                    scale: 1.0,
                    valign: VerticalAlign::Baseline,
                },
                SectionSpec {
                    text: "b",
                    fonts: 0..1,
                    scale: 1.0,
                    valign,
                },
            ],
            &fonts,
            &no_wrap(),
        )
    };
    let ys =
        |b: &ezu_core::text::TextBlock| b.glyphs.iter().map(|g| g.y.to_bits()).collect::<Vec<_>>();
    let baseline = ys(&block(VerticalAlign::Baseline));
    for v in [
        VerticalAlign::Top,
        VerticalAlign::Center,
        VerticalAlign::Bottom,
    ] {
        assert_eq!(
            ys(&block(v)),
            baseline,
            "single-scale line must ignore {v:?}"
        );
    }
}
