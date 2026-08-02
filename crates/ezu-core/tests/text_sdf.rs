//! SDF glyph backend tests: fontnik PBF decode (against bytes built by
//! the tiny encoder below and against a vendored real range — see
//! `tests/glyphs/README.md`), MapLibre metrics, lazy range fetching,
//! and the SDF draw passes.

#![cfg(feature = "text")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ezu_core::text::{
    decode_glyph_range, draw, layout, FaceEntry, Font, LayoutParams, SdfFontStack, SdfGlyph,
    StackEntry, TextPaint,
};

const REAL_RANGE: &[u8] = include_bytes!("glyphs/0-255.pbf");
const LATIN: &[u8] = include_bytes!("fonts/NotoSans-Regular.latin.ttf");

// --- tiny fontnik encoder (test-only) ----------------------------------------

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

fn put_field_varint(out: &mut Vec<u8>, field: u64, v: u64) {
    put_varint(out, field << 3);
    put_varint(out, v);
}

fn put_field_sint(out: &mut Vec<u8>, field: u64, v: i32) {
    let zigzag = ((v << 1) ^ (v >> 31)) as u32;
    put_field_varint(out, field, u64::from(zigzag));
}

fn put_field_bytes(out: &mut Vec<u8>, field: u64, bytes: &[u8]) {
    put_varint(out, (field << 3) | 2);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn encode_glyph(g: &SdfGlyph) -> Vec<u8> {
    let mut out = Vec::new();
    put_field_varint(&mut out, 1, u64::from(g.id));
    if !g.bitmap.is_empty() {
        put_field_bytes(&mut out, 2, &g.bitmap);
    }
    put_field_varint(&mut out, 3, u64::from(g.width));
    put_field_varint(&mut out, 4, u64::from(g.height));
    put_field_sint(&mut out, 5, g.left);
    put_field_sint(&mut out, 6, g.top);
    put_field_varint(&mut out, 7, u64::from(g.advance));
    out
}

fn encode_range(fontstack: &str, range: &str, glyphs: &[SdfGlyph]) -> Vec<u8> {
    let mut stack = Vec::new();
    put_field_bytes(&mut stack, 1, fontstack.as_bytes());
    put_field_bytes(&mut stack, 2, range.as_bytes());
    for g in glyphs {
        put_field_bytes(&mut stack, 3, &encode_glyph(g));
    }
    let mut out = Vec::new();
    put_field_bytes(&mut out, 1, &stack);
    out
}

// --- synthetic glyphs ---------------------------------------------------------

/// An SDF whose ink is the full `width × height` box: field value 0.75
/// on the box edge, falling off 1/8 per px outside (the fontnik
/// radius-8 / cutoff-0.25 encoding).
fn box_sdf(width: u32, height: u32) -> Vec<u8> {
    let (bw, bh) = (width + 6, height + 6);
    let (x0, y0) = (3.0f32, 3.0f32);
    let (x1, y1) = (3.0 + width as f32, 3.0 + height as f32);
    let mut bitmap = Vec::with_capacity((bw * bh) as usize);
    for y in 0..bh {
        for x in 0..bw {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let dx = (x0 - px).max(px - x1);
            let dy = (y0 - py).max(py - y1);
            let d = if dx > 0.0 || dy > 0.0 {
                (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt()
            } else {
                dx.max(dy)
            };
            bitmap.push(((0.75 - d / 8.0).clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    bitmap
}

/// A 10×12 SDF box under an arbitrary codepoint.
fn box_glyph(id: u32) -> SdfGlyph {
    SdfGlyph {
        id,
        bitmap: box_sdf(10, 12),
        width: 10,
        height: 12,
        left: 1,
        top: -6,
        advance: 13,
    }
}

/// 'A' as a 10×12 SDF box, 'B' as an inkless advance (space-like).
fn test_glyphs() -> Vec<SdfGlyph> {
    vec![
        SdfGlyph {
            id: 'A' as u32,
            bitmap: box_sdf(10, 12),
            width: 10,
            height: 12,
            left: 1,
            top: -6,
            advance: 13,
        },
        SdfGlyph {
            id: 'B' as u32,
            bitmap: Vec::new(),
            width: 0,
            height: 0,
            left: 0,
            top: 0,
            advance: 7,
        },
    ]
}

fn test_stack() -> Arc<SdfFontStack> {
    let stack = SdfFontStack::new();
    stack
        .insert_range(&encode_range("Test Sans", "0-255", &test_glyphs()))
        .expect("synthetic range decodes");
    Arc::new(stack)
}

fn no_wrap() -> LayoutParams {
    LayoutParams {
        max_width_em: 0.0,
        ..LayoutParams::default()
    }
}

// --- PBF decode ---------------------------------------------------------------

#[test]
fn synthesized_pbf_round_trips() {
    let bytes = encode_range("Test Sans", "0-255", &test_glyphs());
    let decoded = decode_glyph_range(&bytes).expect("decodes");
    assert_eq!(decoded.fontstack, "Test Sans");
    assert_eq!((decoded.start, decoded.end), (0, 255));
    assert_eq!(decoded.glyphs.len(), 2);

    let a = &decoded.glyphs[0];
    assert_eq!(a.id, 'A' as u32);
    assert_eq!((a.width, a.height), (10, 12));
    assert_eq!((a.left, a.top), (1, -6));
    assert_eq!(a.advance, 13);
    assert_eq!(a.bitmap, box_sdf(10, 12));

    let b = &decoded.glyphs[1];
    assert_eq!(b.id, 'B' as u32);
    assert!(b.bitmap.is_empty());
    assert_eq!(b.advance, 7);
}

#[test]
fn malformed_pbf_is_rejected() {
    assert!(decode_glyph_range(&[]).is_err());
    // A bitmap whose length disagrees with (width+6)×(height+6).
    let bad = SdfGlyph {
        bitmap: vec![0; 5],
        ..test_glyphs()[0].clone()
    };
    let bytes = encode_range("Test Sans", "0-255", &[bad]);
    assert!(decode_glyph_range(&bytes).is_err());
}

#[test]
fn vendored_range_decodes() {
    let decoded = decode_glyph_range(REAL_RANGE).expect("real range decodes");
    assert_eq!(decoded.fontstack, "Klokantech Noto Sans Regular");
    assert_eq!((decoded.start, decoded.end), (0, 255));
    let a = decoded
        .glyphs
        .iter()
        .find(|g| g.id == 'A' as u32)
        .expect("'A' is present");
    assert!(a.advance > 0);
    assert_eq!(
        a.bitmap.len() as u32,
        (a.width + 6) * (a.height + 6),
        "bitmap spans the ink box plus the 3 px border"
    );
}

// --- shaping & layout ----------------------------------------------------------

#[test]
fn shaping_advances_match_the_pbf() {
    let fonts = [StackEntry::Sdf(test_stack())];
    let fonts = FaceEntry::prepare(&fonts);
    let block = layout("AB", &fonts, &no_wrap());
    assert_eq!(block.glyphs.len(), 2);
    // Pen advance between the glyphs is 'A''s PBF advance at the 24 px em.
    let dx = block.glyphs[1].x - block.glyphs[0].x;
    assert!(
        (dx - 13.0 / 24.0).abs() < 1e-5,
        "advance should be 13/24 em, got {dx}"
    );
}

#[test]
fn an_unwrapped_block_still_breaks_at_a_newline() {
    // Line placement lays a label out un-wrapped, but an explicit `\n` is a
    // mandatory break there too — a road name and its translation stack along
    // the path rather than running together on one line.
    let fonts = [StackEntry::Sdf(test_stack())];
    let fonts = FaceEntry::prepare(&fonts);
    let params = no_wrap();
    let one = layout("AB", &fonts, &params);
    let two = layout("AB\nAB", &fonts, &params);
    assert_eq!(two.glyphs.len(), 4);
    assert!(
        (two.bbox.height() - 2.0 * params.line_height_em).abs() < 1e-5,
        "the newline should open a second line slot: {:?}",
        two.bbox
    );
    assert!(
        (two.bbox.width() - one.bbox.width()).abs() < 1e-5,
        "both lines are the same run, so the block stays one line wide: {:?}",
        two.bbox
    );
}

#[test]
fn sdf_metrics_use_the_fixed_line_slot() {
    // MapLibre metrics: a block is `line-height × lines` tall, however
    // tall the glyphs actually are.
    let fonts = [StackEntry::Sdf(test_stack())];
    let fonts = FaceEntry::prepare(&fonts);
    let params = no_wrap();
    let block = layout("AB", &fonts, &params);
    assert!(
        (block.bbox.height() - params.line_height_em).abs() < 1e-5,
        "single-line block should be exactly one line slot: {:?}",
        block.bbox
    );
}

#[test]
fn a_newline_splits_the_block_into_two_line_slots() {
    // The glyph protocol has no glyph for `\n`; the break is structural.
    let fonts = [StackEntry::Sdf(test_stack())];
    let fonts = FaceEntry::prepare(&fonts);
    let params = LayoutParams {
        max_width_em: 20.0,
        ..LayoutParams::default()
    };
    let one = layout("A", &fonts, &params);
    let two = layout("A\nA", &fonts, &params);
    assert_eq!(two.glyphs.len(), 2);
    assert!(
        (two.bbox.height() - 2.0 * params.line_height_em).abs() < 1e-5,
        "two lines should be two line slots tall: {:?}",
        two.bbox
    );
    assert!(
        (two.bbox.width() - one.bbox.width()).abs() < 1e-5,
        "the block should be one line wide: {} vs {}",
        two.bbox.width(),
        one.bbox.width()
    );
}

#[test]
fn a_second_line_without_glyphs_keeps_its_slot_but_adds_no_width() {
    // Bilingual label whose local-name range is absent: the first line's
    // width is the whole block's, and the empty line still occupies a
    // slot (maplibre-gl-js `shapeLines`).
    let fonts = [StackEntry::Sdf(test_stack())];
    let fonts = FaceEntry::prepare(&fonts);
    let params = LayoutParams {
        max_width_em: 20.0,
        ..LayoutParams::default()
    };
    let one = layout("A", &fonts, &params);
    let block = layout("A\n目黒区", &fonts, &params);
    assert_eq!(block.glyphs.len(), 1);
    assert!(
        (block.bbox.width() - one.bbox.width()).abs() < 1e-5,
        "width should not include the missing line: {:?}",
        block.bbox
    );
    assert!((block.bbox.height() - 2.0 * params.line_height_em).abs() < 1e-5);
}

#[test]
fn missing_range_without_fetcher_drops_and_counts() {
    let fonts = [StackEntry::Sdf(Arc::new(SdfFontStack::new()))];
    let fonts = FaceEntry::prepare(&fonts);
    let block = layout("Hi", &fonts, &no_wrap());
    assert!(block.is_empty());
    assert_eq!(block.dropped_chars, 2);
    assert_eq!(block.missing_range_chars, 2);
}

#[test]
fn fetcher_pulls_each_range_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let stack = Arc::new(SdfFontStack::with_fetcher(Box::new(move |start, end| {
        counted.fetch_add(1, Ordering::SeqCst);
        if start == 0 {
            Ok(encode_range(
                "Test Sans",
                &format!("{start}-{end}"),
                &test_glyphs(),
            ))
        } else {
            Err("no such range".into())
        }
    })));
    let fonts = [StackEntry::Sdf(stack.clone())];
    let fonts = FaceEntry::prepare(&fonts);

    // Two layouts over the same range: one fetch.
    assert!(!layout("AB", &fonts, &no_wrap()).is_empty());
    assert!(!layout("BA", &fonts, &no_wrap()).is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(stack.is_loaded(0));

    // A failing range is fetched once, then remembered; its chars count
    // as missing-range drops.
    let block = layout("あ", &fonts, &no_wrap());
    assert!(block.is_empty());
    assert_eq!(block.missing_range_chars, 1);
    let block = layout("あ", &fonts, &no_wrap());
    assert_eq!(block.missing_range_chars, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_subset_pbf_files_each_glyph_by_its_own_id() {
    // A host that builds its own PBF (from `neededCodepoints`) ships
    // only the characters a tile draws, in one message spanning as many
    // blocks as it likes. Every glyph must still resolve, whatever the
    // `range` string says.
    let stack = SdfFontStack::new();
    stack
        .insert_range(&encode_range(
            "Test Sans",
            "0-65535",
            &[box_glyph('A' as u32), box_glyph('あ' as u32)],
        ))
        .expect("subset decodes");
    assert!(stack.is_loaded(0), "block 0 holds 'A'");
    assert!(stack.is_loaded(0x30), "block 0x30 holds 'あ'");
    assert!(stack.glyph('あ').is_some(), "'あ' must not follow 'A' home");

    let fonts = [StackEntry::Sdf(Arc::new(stack))];
    let fonts = FaceEntry::prepare(&fonts);
    let block = layout("Aあ", &fonts, &no_wrap());
    assert_eq!(block.glyphs.len(), 2);
    assert_eq!(block.dropped_chars, 0);
}

#[test]
fn partial_binds_of_one_block_accumulate() {
    // Two subsets can each carry part of the same block; the second
    // must not evict the first, and the hash has to move so caches
    // keyed on it see the wider coverage.
    let stack = SdfFontStack::new();
    stack
        .insert_range(&encode_range(
            "Test Sans",
            "0-255",
            &[box_glyph('A' as u32)],
        ))
        .unwrap();
    let after_first = stack.ranges_hash();
    stack
        .insert_range(&encode_range(
            "Test Sans",
            "0-255",
            &[box_glyph('C' as u32)],
        ))
        .unwrap();
    assert!(stack.glyph('A').is_some(), "the earlier glyph survives");
    assert!(stack.glyph('C').is_some());
    assert_ne!(
        stack.ranges_hash(),
        after_first,
        "widening a block must rehash"
    );
}

#[test]
fn an_empty_whole_range_still_counts_as_loaded() {
    // A range a server has nothing for is a fact worth remembering:
    // without a glyph to file it under, the block would look unseen and
    // a fetcher would ask again on every layout.
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let stack = SdfFontStack::with_fetcher(Box::new(move |start, end| {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(encode_range("Test Sans", &format!("{start}-{end}"), &[]))
    }));
    let fonts = [StackEntry::Sdf(Arc::new(stack))];
    let fonts = FaceEntry::prepare(&fonts);
    assert!(layout("AA", &fonts, &no_wrap()).is_empty());
    assert!(layout("AA", &fonts, &no_wrap()).is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn trimming_drops_the_coldest_blocks_first() {
    let stack = SdfFontStack::new();
    for (block, id) in [(0u32, 'A' as u32), (0x30, 'あ' as u32), (0x4E, 0x4E00)] {
        let start = block << 8;
        stack
            .insert_range(&encode_range(
                "Test Sans",
                &format!("{start}-{}", start + 255),
                &[box_glyph(id)],
            ))
            .unwrap();
    }
    let (blocks, bytes) = stack.loaded_size();
    assert_eq!(blocks, 3);

    // Read the oldest block back, making the second-oldest the coldest.
    assert!(stack.glyph('A').is_some());

    // Budget for two of the three.
    stack.set_byte_budget(bytes * 2 / 3);
    assert_eq!(stack.trim_to_budget(), 1, "one block should go");
    assert!(stack.glyph('あ').is_none(), "the coldest block was dropped");
    assert!(stack.glyph('A').is_some(), "the block just read survives");
    assert!(stack.glyph('\u{4E00}').is_some(), "so does the newest");
    assert!(stack.loaded_size().1 <= bytes * 2 / 3);
}

#[test]
fn an_unlimited_budget_keeps_everything() {
    let stack = SdfFontStack::new();
    stack
        .insert_range(&encode_range("Test Sans", "0-255", &test_glyphs()))
        .unwrap();
    assert_eq!(stack.byte_budget(), usize::MAX, "unlimited by default");
    assert_eq!(stack.trim_to_budget(), 0);
    assert!(stack.glyph('A').is_some());
}

#[test]
fn a_budget_below_one_block_is_still_honoured() {
    // The ceiling is what the host asked for, so it holds even when it
    // empties the stack. Nothing is lost mid-render — trimming runs
    // after one — and the next tile re-binds what it needs.
    let stack = SdfFontStack::new();
    stack
        .insert_range(&encode_range("Test Sans", "0-255", &test_glyphs()))
        .unwrap();
    stack.set_byte_budget(1);
    assert_eq!(stack.trim_to_budget(), 1);
    assert_eq!(stack.loaded_size(), (0, 0));
    assert!(stack.glyph('A').is_none());
}

#[test]
fn ranges_hash_tracks_the_loaded_set() {
    let stack = SdfFontStack::new();
    let empty = stack.ranges_hash();
    stack
        .insert_range(&encode_range("Test Sans", "0-255", &test_glyphs()))
        .unwrap();
    assert_ne!(stack.ranges_hash(), empty, "loading a range must rehash");
    assert_eq!(SdfFontStack::blocks_for("Aあ"), vec![0, 0x30]);
}

#[test]
fn outline_and_sdf_entries_mix_in_one_stack() {
    // The latin outline subset has no digits; the vendored SDF range does.
    let outline = Arc::new(Font::from_bytes(Arc::from(LATIN), 0).expect("latin parses"));
    let sdf = Arc::new(SdfFontStack::new());
    sdf.insert_range(REAL_RANGE).unwrap();
    let fonts = [StackEntry::Outline(outline), StackEntry::Sdf(sdf)];
    let fonts = FaceEntry::prepare(&fonts);
    let block = layout("A1", &fonts, &no_wrap());
    let picked: Vec<usize> = block.glyphs.iter().map(|g| g.font).collect();
    assert_eq!(picked, [0, 1], "'A' shapes outline, '1' falls to the SDF");
    assert_eq!(block.dropped_chars, 0);
}

// --- drawing -------------------------------------------------------------------

fn render_sdf(text: &str, size_px: f32, halo_width_px: f32) -> tiny_skia::Pixmap {
    let fonts = [StackEntry::Sdf(test_stack())];
    let fonts = FaceEntry::prepare(&fonts);
    let block = layout(text, &fonts, &no_wrap());
    let mut pixmap = tiny_skia::Pixmap::new(96, 64).unwrap();
    draw(
        &block,
        &fonts,
        &mut pixmap.as_mut(),
        (48.0, 32.0),
        &TextPaint {
            size_px,
            color: [1.0, 0.0, 0.0, 1.0],
            halo_color: [1.0, 1.0, 1.0, 1.0],
            halo_width_px,
            halo_blur_px: 0.0,
        },
        &[],
        None,
    );
    pixmap
}

#[test]
fn sdf_draw_fills_the_glyph() {
    let pixmap = render_sdf("A", 24.0, 0.0);
    let red = pixmap
        .pixels()
        .iter()
        .filter(|p| p.alpha() == 255 && p.red() > 200 && p.green() < 50)
        .count();
    // The synthetic ink box is 10×12 px at the native 24 px em.
    assert!(red > 60, "expected solid red SDF fill, got {red}");
}

#[test]
fn sdf_draw_scales_with_bilinear_sampling() {
    let native = render_sdf("A", 24.0, 0.0);
    let scaled = render_sdf("A", 48.0, 0.0);
    let count = |p: &tiny_skia::Pixmap| {
        p.pixels()
            .iter()
            .filter(|p| p.alpha() == 255 && p.red() > 200)
            .count()
    };
    let (n, s) = (count(&native), count(&scaled));
    assert!(
        s > 3 * n,
        "2× font scale should roughly quadruple the fill: {n} → {s}"
    );
}

#[test]
fn sdf_halo_lies_outside_the_fill() {
    let fill_only = render_sdf("A", 24.0, 0.0);
    let with_halo = render_sdf("A", 24.0, 2.0);
    // The halo adds white coverage on pixels the fill left empty …
    let halo_added = fill_only
        .pixels()
        .iter()
        .zip(with_halo.pixels())
        .filter(|(a, b)| a.alpha() == 0 && b.alpha() > 200 && b.green() > 150)
        .count();
    assert!(
        halo_added > 20,
        "expected white halo ring, got {halo_added}"
    );
    // … and never repaints the solid fill interior (fill pass runs last).
    for (a, b) in fill_only.pixels().iter().zip(with_halo.pixels()) {
        if a.alpha() == 255 && a.red() > 200 && a.green() < 10 {
            assert!(
                b.red() > 200 && b.green() < 60,
                "halo overpainted a fill pixel: {a:?} → {b:?}"
            );
        }
    }
}

#[test]
fn vendored_range_renders_a_word() {
    let sdf = Arc::new(SdfFontStack::new());
    sdf.insert_range(REAL_RANGE).unwrap();
    let fonts = [StackEntry::Sdf(sdf)];
    let fonts = FaceEntry::prepare(&fonts);
    let block = layout("Word", &fonts, &no_wrap());
    assert_eq!(block.glyphs.len(), 4);
    assert_eq!(block.dropped_chars, 0);
    let mut pixmap = tiny_skia::Pixmap::new(96, 64).unwrap();
    draw(
        &block,
        &fonts,
        &mut pixmap.as_mut(),
        (48.0, 32.0),
        &TextPaint {
            size_px: 24.0,
            color: [0.0, 0.0, 0.0, 1.0],
            halo_color: [1.0, 1.0, 1.0, 1.0],
            halo_width_px: 1.0,
            halo_blur_px: 0.0,
        },
        &[],
        None,
    );
    let ink = pixmap.pixels().iter().filter(|p| p.alpha() > 100).count();
    assert!(ink > 100, "expected a rendered word, got {ink} inked px");
}
