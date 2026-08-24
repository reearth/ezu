//! Right-to-left layout: a label's glyphs come out of `layout` in the
//! order they are painted, left to right, whatever order its chars were
//! written in (UAX #9), and an Arabic one is drawn as the joined
//! presentation forms its letters call for.
//!
//! Driven through the SDF backend over a synthetic glyph stack, so the
//! expectations are about ordering and codepoint choice alone and no
//! font's own coverage or shaping is in the way. The stack is stocked
//! explicitly per test, which is also how the fallback to unjoined
//! letterforms is exercised: leave the presentation forms out of it.

#![cfg(feature = "text")]

use std::sync::Arc;

use ezu_core::text::{layout, FaceEntry, LayoutParams, SdfFontStack, StackEntry};

mod glyph_pbf;
use glyph_pbf::{box_glyph, encode_range};

/// A glyph stack carrying `codepoints`, each an identical box, so every
/// one is present and none is wider than another.
fn stack_of(codepoints: impl IntoIterator<Item = u32>) -> Arc<SdfFontStack> {
    let stack = SdfFontStack::new();
    let mut by_block: std::collections::BTreeMap<u32, Vec<u32>> = std::collections::BTreeMap::new();
    for cp in codepoints {
        by_block.entry(cp >> 8).or_default().push(cp);
    }
    for (block, codepoints) in by_block {
        let glyphs: Vec<_> = codepoints.into_iter().map(box_glyph).collect();
        let range = format!("{}-{}", block * 256, block * 256 + 255);
        stack
            .insert_range(&encode_range("Test Sans", &range, &glyphs))
            .expect("synthetic range decodes");
    }
    Arc::new(stack)
}

/// A glyph stack covering every char of `text`.
fn stack_over(text: &str) -> Arc<SdfFontStack> {
    stack_of(text.chars().map(|c| c as u32))
}

fn no_wrap() -> LayoutParams {
    LayoutParams {
        max_width_em: 0.0,
        ..LayoutParams::default()
    }
}

/// `text` laid out over a stack covering it, read back as the chars in
/// the order they are painted, leftmost first.
fn visual_order(text: &str) -> String {
    let fonts = [StackEntry::Sdf(stack_over(text))];
    let fonts = FaceEntry::prepare(&fonts);
    let block = layout(text, &fonts, &no_wrap());
    let mut placed: Vec<_> = block.glyphs.iter().collect();
    // The block should already be in visual order; sorting by pen
    // position asserts that separately below.
    placed.sort_by(|a, b| a.x.partial_cmp(&b.x).expect("finite pen positions"));
    placed
        .iter()
        .map(|g| char::from_u32(u32::from(g.glyph_id)).expect("a BMP codepoint"))
        .collect()
}

#[test]
fn a_hebrew_label_is_painted_right_to_left() {
    assert_eq!(visual_order("תל אביב"), "ביבא לת");
}

#[test]
fn a_latin_label_is_untouched() {
    assert_eq!(visual_order("Tel Aviv"), "Tel Aviv");
}

#[test]
fn layout_emits_glyphs_in_visual_order() {
    let text = "תל אביב";
    let fonts = [StackEntry::Sdf(stack_over(text))];
    let fonts = FaceEntry::prepare(&fonts);
    let block = layout(text, &fonts, &no_wrap());
    assert!(
        block.glyphs.windows(2).all(|w| w[0].x <= w[1].x),
        "glyphs are emitted left to right, not in logical order"
    );
}

#[test]
fn a_right_to_left_run_inside_a_latin_sentence_reverses_alone() {
    // Paragraph direction is Latin, so the Hebrew word is an island: it
    // reverses, the sentence around it does not.
    assert_eq!(visual_order("in תל today"), "in לת today");
}

#[test]
fn a_latin_run_inside_a_right_to_left_sentence_reverses_alone() {
    assert_eq!(visual_order("תל Aviv אביב"), "ביבא Aviv לת");
}

#[test]
fn digits_inside_a_right_to_left_run_keep_reading_left_to_right() {
    // The number is a level-2 island inside the level-1 Hebrew: it
    // lands at the sentence's left end with its digits in order.
    assert_eq!(visual_order("אב12"), "12בא");
}

#[test]
fn each_wrapped_line_reorders_on_its_own() {
    let text = "תל\nאביב";
    let fonts = [StackEntry::Sdf(stack_over(text))];
    let fonts = FaceEntry::prepare(&fonts);
    let block = layout(text, &fonts, &no_wrap());
    // Group by baseline, then read each line left to right.
    let mut lines: std::collections::BTreeMap<i32, Vec<(f32, char)>> = Default::default();
    for g in &block.glyphs {
        lines
            .entry((g.y * 1000.0) as i32)
            .or_default()
            .push((g.x, char::from_u32(u32::from(g.glyph_id)).unwrap()));
    }
    let read: Vec<String> = lines
        .into_values()
        .map(|mut row| {
            row.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            row.into_iter().map(|(_, c)| c).collect()
        })
        .collect();
    assert_eq!(read, vec!["לת".to_string(), "ביבא".to_string()]);
}

// --- Arabic joining -----------------------------------------------------------

/// The codepoints `text` draws as, over a stack carrying `stocked`,
/// leftmost glyph first.
fn drawn_over(text: &str, stocked: impl IntoIterator<Item = u32>) -> Vec<u32> {
    let fonts = [StackEntry::Sdf(stack_of(stocked))];
    let fonts = FaceEntry::prepare(&fonts);
    let block = layout(text, &fonts, &no_wrap());
    block.glyphs.iter().map(|g| u32::from(g.glyph_id)).collect()
}

/// The Arabic block plus both presentation-form blocks — what a host
/// serving a MapLibre glyphs endpoint would bind for an Arabic label.
fn arabic_and_forms() -> impl Iterator<Item = u32> {
    (0x0600..=0x06FF)
        .chain(0xFB50..=0xFDFF)
        .chain(0xFE70..=0xFEFF)
}

#[test]
fn an_arabic_word_draws_as_its_joined_forms() {
    // الورود (Al Wurud): alef opens it unjoined, so the lam that follows
    // is initial and the waw after that final; each letter following a
    // right-joining one starts over isolated.
    assert_eq!(
        drawn_over("الورود", arabic_and_forms()),
        vec![
            0xFEA9, // dal isolated
            0xFEED, // waw isolated
            0xFEAD, // reh isolated
            0xFEEE, // waw final
            0xFEDF, // lam initial
            0xFE8D, // alef isolated
        ],
        "read right to left: alef, lam, waw, reh, waw, dal"
    );
}

#[test]
fn a_stack_without_the_presentation_forms_falls_back_to_the_letters() {
    // The Arabic block alone: unjoined, as before, rather than nothing.
    assert_eq!(
        drawn_over("الورود", 0x0600..=0x06FF),
        vec![0x062F, 0x0648, 0x0631, 0x0648, 0x0644, 0x0627]
    );
}

#[test]
fn lam_alef_draws_as_one_ligature() {
    assert_eq!(drawn_over("لا", arabic_and_forms()), vec![0xFEFB]);
    // Joined to a beh before it, the pair takes its final shape.
    assert_eq!(
        drawn_over("بلا", arabic_and_forms()),
        vec![0xFEFC, 0xFE91],
        "beh initial, then the final lam-alef"
    );
}

#[test]
fn a_mark_between_two_letters_does_not_break_their_join() {
    // بَت — the fatha sits over the beh, which still joins to the teh.
    assert_eq!(
        drawn_over("بَت", arabic_and_forms()),
        vec![0xFE96, 0x064E, 0xFE91],
        "teh final, the mark, beh initial"
    );
}

#[test]
fn an_arabic_run_is_drawn_without_letter_spacing() {
    let fonts = [StackEntry::Sdf(stack_of(arabic_and_forms()))];
    let fonts = FaceEntry::prepare(&fonts);
    let spaced = LayoutParams {
        letter_spacing_em: 0.5,
        ..no_wrap()
    };
    let arabic = layout("الورود", &fonts, &spaced);
    assert_eq!(
        arabic.bbox.width(),
        layout("الورود", &fonts, &no_wrap()).bbox.width(),
        "letter spacing would open gaps inside the joined word"
    );
}
