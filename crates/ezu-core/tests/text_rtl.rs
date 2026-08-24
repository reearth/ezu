//! Right-to-left layout: a label's glyphs come out of `layout` in the
//! order they are painted, left to right, whatever order its chars were
//! written in (UAX #9).
//!
//! Driven through the SDF backend over a synthetic glyph stack, so the
//! expectations are about ordering alone and no font's own coverage or
//! shaping is in the way.

#![cfg(feature = "text")]

use std::sync::Arc;

use ezu_core::text::{layout, FaceEntry, LayoutParams, SdfFontStack, StackEntry};

mod glyph_pbf;
use glyph_pbf::{box_glyph, encode_range};

/// A glyph stack covering every char of `text` with an identical box, so
/// each codepoint is present and none is wider than another.
fn stack_over(text: &str) -> Arc<SdfFontStack> {
    let stack = SdfFontStack::new();
    let mut by_block: std::collections::BTreeMap<u32, Vec<u32>> = std::collections::BTreeMap::new();
    for c in text.chars() {
        by_block.entry(c as u32 >> 8).or_default().push(c as u32);
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
