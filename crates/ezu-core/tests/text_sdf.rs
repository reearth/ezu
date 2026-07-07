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
    );
    let ink = pixmap.pixels().iter().filter(|p| p.alpha() > 100).count();
    assert!(ink > 100, "expected a rendered word, got {ink} inked px");
}
