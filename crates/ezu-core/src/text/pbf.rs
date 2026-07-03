//! Fontnik glyph-PBF decoding.
//!
//! A glyph range (`…/{fontstack}/{range}.pbf`) is a tiny protobuf
//! message — three message types, seven scalar fields:
//!
//! ```proto
//! message glyphs    { repeated fontstack stacks = 1; }
//! message fontstack { required string name = 1; required string range = 2;
//!                     repeated glyph glyphs = 3; }
//! message glyph     { required uint32 id = 1;      optional bytes bitmap = 2;
//!                     required uint32 width = 3;   required uint32 height = 4;
//!                     required sint32 left = 5;    required sint32 top = 6;
//!                     required uint32 advance = 7; }
//! ```
//!
//! The schema is small and frozen, so the wire format is read with a
//! hand-rolled varint/length-delimited reader rather than a protobuf
//! dependency.

use super::sdf::{SdfGlyph, SDF_BORDER};

/// Errors decoding a glyph-range PBF.
#[derive(Debug, thiserror::Error)]
pub enum GlyphPbfError {
    #[error("glyph pbf: truncated message")]
    Truncated,
    #[error("glyph pbf: {0}")]
    Malformed(&'static str),
}

/// One decoded glyph range: the fontstack name the server resolved, the
/// `start-end` codepoint bounds, and the glyphs present in the range.
#[derive(Debug)]
pub struct GlyphRange {
    pub fontstack: String,
    pub start: u32,
    pub end: u32,
    pub glyphs: Vec<SdfGlyph>,
}

/// Decode a glyph-range PBF. A response carries one `fontstack` message
/// (server-side fallback is already merged); extra stacks are ignored.
pub fn decode_glyph_range(bytes: &[u8]) -> Result<GlyphRange, GlyphPbfError> {
    let mut r = Reader::new(bytes);
    while let Some((field, wire)) = r.next_field()? {
        if field == 1 && wire == WIRE_LEN {
            return decode_fontstack(r.bytes()?);
        }
        r.skip(wire)?;
    }
    Err(GlyphPbfError::Malformed("no fontstack message"))
}

fn decode_fontstack(bytes: &[u8]) -> Result<GlyphRange, GlyphPbfError> {
    let mut r = Reader::new(bytes);
    let mut fontstack = String::new();
    let mut range: Option<(u32, u32)> = None;
    let mut glyphs = Vec::new();
    while let Some((field, wire)) = r.next_field()? {
        match (field, wire) {
            (1, WIRE_LEN) => {
                fontstack = std::str::from_utf8(r.bytes()?)
                    .map_err(|_| GlyphPbfError::Malformed("fontstack name is not UTF-8"))?
                    .to_string();
            }
            (2, WIRE_LEN) => {
                let s = std::str::from_utf8(r.bytes()?)
                    .map_err(|_| GlyphPbfError::Malformed("range is not UTF-8"))?;
                let (start, end) = s
                    .split_once('-')
                    .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
                    .ok_or(GlyphPbfError::Malformed("range is not `start-end`"))?;
                range = Some((start, end));
            }
            (3, WIRE_LEN) => glyphs.push(decode_glyph(r.bytes()?)?),
            _ => r.skip(wire)?,
        }
    }
    let (start, end) = range.ok_or(GlyphPbfError::Malformed("fontstack has no range"))?;
    Ok(GlyphRange {
        fontstack,
        start,
        end,
        glyphs,
    })
}

fn decode_glyph(bytes: &[u8]) -> Result<SdfGlyph, GlyphPbfError> {
    let mut r = Reader::new(bytes);
    let mut g = SdfGlyph {
        id: 0,
        bitmap: Vec::new(),
        width: 0,
        height: 0,
        left: 0,
        top: 0,
        advance: 0,
    };
    while let Some((field, wire)) = r.next_field()? {
        match (field, wire) {
            (1, WIRE_VARINT) => g.id = r.varint()? as u32,
            (2, WIRE_LEN) => g.bitmap = r.bytes()?.to_vec(),
            (3, WIRE_VARINT) => g.width = r.varint()? as u32,
            (4, WIRE_VARINT) => g.height = r.varint()? as u32,
            (5, WIRE_VARINT) => g.left = zigzag(r.varint()?),
            (6, WIRE_VARINT) => g.top = zigzag(r.varint()?),
            (7, WIRE_VARINT) => g.advance = r.varint()? as u32,
            _ => r.skip(wire)?,
        }
    }
    // The bitmap spans the ink box plus the baked border on every side.
    let expected = ((g.width + 2 * SDF_BORDER) * (g.height + 2 * SDF_BORDER)) as usize;
    if !g.bitmap.is_empty() && g.bitmap.len() != expected {
        return Err(GlyphPbfError::Malformed("bitmap size mismatch"));
    }
    Ok(g)
}

const WIRE_VARINT: u8 = 0;
const WIRE_LEN: u8 = 2;

fn zigzag(v: u64) -> i32 {
    ((v >> 1) as i64 ^ -((v & 1) as i64)) as i32
}

/// Minimal protobuf wire reader: varints, field headers, and
/// length-delimited payloads; unknown wire types are skipped.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    /// The next `(field number, wire type)` header, or `None` at the end.
    fn next_field(&mut self) -> Result<Option<(u32, u8)>, GlyphPbfError> {
        if self.pos >= self.buf.len() {
            return Ok(None);
        }
        let key = self.varint()?;
        Ok(Some(((key >> 3) as u32, (key & 0x7) as u8)))
    }

    fn varint(&mut self) -> Result<u64, GlyphPbfError> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.buf.get(self.pos).ok_or(GlyphPbfError::Truncated)?;
            self.pos += 1;
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(GlyphPbfError::Malformed("varint overruns 64 bits"))
    }

    fn bytes(&mut self) -> Result<&'a [u8], GlyphPbfError> {
        let len = self.varint()? as usize;
        let end = self.pos.checked_add(len).ok_or(GlyphPbfError::Truncated)?;
        if end > self.buf.len() {
            return Err(GlyphPbfError::Truncated);
        }
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn skip(&mut self, wire: u8) -> Result<(), GlyphPbfError> {
        match wire {
            WIRE_VARINT => {
                self.varint()?;
            }
            1 => self.advance(8)?,
            WIRE_LEN => {
                self.bytes()?;
            }
            5 => self.advance(4)?,
            _ => return Err(GlyphPbfError::Malformed("unknown wire type")),
        }
        Ok(())
    }

    fn advance(&mut self, n: usize) -> Result<(), GlyphPbfError> {
        let end = self.pos.checked_add(n).ok_or(GlyphPbfError::Truncated)?;
        if end > self.buf.len() {
            return Err(GlyphPbfError::Truncated);
        }
        self.pos = end;
        Ok(())
    }
}
