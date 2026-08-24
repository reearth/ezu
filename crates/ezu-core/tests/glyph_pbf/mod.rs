//! A tiny fontnik glyph-PBF encoder and synthetic glyphs, so a test can
//! build a `SdfFontStack` holding exactly the codepoints it cares about
//! without vendoring a font for each one.
//!
//! Shared by the test binaries that need an SDF stack; each uses a
//! subset of it, so `dead_code` is expected here.

#![allow(dead_code)]

use ezu_core::text::SdfGlyph;

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

/// One range PBF holding `glyphs`, labelled with `fontstack` and the
/// `start-end` `range` string.
pub fn encode_range(fontstack: &str, range: &str, glyphs: &[SdfGlyph]) -> Vec<u8> {
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

/// An SDF whose ink is the full `width × height` box: field value 0.75
/// on the box edge, falling off 1/8 per px outside (the fontnik
/// radius-8 / cutoff-0.25 encoding).
pub fn box_sdf(width: u32, height: u32) -> Vec<u8> {
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
pub fn box_glyph(id: u32) -> SdfGlyph {
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
