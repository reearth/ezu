//! Shared helpers for built-in node implementations.

use std::any::Any;
use std::sync::Arc;

use ezu_core::TileId as CoreTileId;
use ezu_graph::{
    EvalCtx, EvalError, FactoryCtx, FactoryError, PortValue, RasterBuf,
};
use ezu_style as spec;
use hokusai::Brush;
use serde_json::Value;

use crate::Canvas;

// ---------------------------------------------------------------------------
// Concrete payload types for type-erased ports.

/// Payload carried on a `Features` port. Produced by `mvt-source`;
/// consumed by `fill-solid`, `fill-dabs`, `line`.
pub struct FilteredFeatures {
    pub extent: u32,
    pub polygons: Vec<ezu_features::Polygon>,
    pub lines: Vec<Vec<(i32, i32)>>,
}

/// Payload carried on a `Brush` port. Wraps a hokusai brush.
pub type BrushPayload = Brush;

// ---------------------------------------------------------------------------
// Field reading

/// Resolve a node field: if it's a string starting with `$`, look it up
/// in the document's `params`. Returns the resolved JSON value.
pub(super) fn resolve_field(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    ctx: &FactoryCtx<'_>,
) -> Result<Value, FactoryError> {
    let v = fields
        .get(name)
        .ok_or_else(|| FactoryError::MissingField(name.to_string()))?;
    if let Some(s) = v.as_str() {
        match spec::FieldRef::classify(s) {
            spec::FieldRef::Param(p) => {
                let decl = ctx
                    .params
                    .get(p)
                    .ok_or_else(|| FactoryError::UnknownParam(p.to_string()))?;
                return Ok(decl.default.clone());
            }
            spec::FieldRef::Node(_) => {
                return Err(FactoryError::BadField {
                    field: name.into(),
                    msg: "expected literal or $param, got @node-ref".into(),
                });
            }
            spec::FieldRef::Literal(_) => {}
        }
    }
    Ok(v.clone())
}

pub(super) fn read_color(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    ctx: &FactoryCtx<'_>,
) -> Result<[f32; 4], FactoryError> {
    let v = resolve_field(fields, name, ctx)?;
    let s = v.as_str().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "expected #rrggbb[aa] string".into(),
    })?;
    parse_hex_color(s).ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: format!("bad color: {s}"),
    })
}

pub(super) fn read_color_u8(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    ctx: &FactoryCtx<'_>,
) -> Result<[u8; 4], FactoryError> {
    let v = resolve_field(fields, name, ctx)?;
    let s = v.as_str().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "expected #rrggbb[aa] string".into(),
    })?;
    parse_hex_color_u8(s).ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: format!("bad color: {s}"),
    })
}

pub(super) fn read_number(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    ctx: &FactoryCtx<'_>,
) -> Result<f64, FactoryError> {
    let v = resolve_field(fields, name, ctx)?;
    v.as_f64().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "expected number".into(),
    })
}

pub(super) fn read_number_or(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    ctx: &FactoryCtx<'_>,
    default: f64,
) -> Result<f64, FactoryError> {
    if !fields.contains_key(name) {
        return Ok(default);
    }
    read_number(fields, name, ctx)
}

pub(super) fn read_optional_string(
    fields: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<Option<String>, FactoryError> {
    let Some(v) = fields.get(name) else {
        return Ok(None);
    };
    let s = v.as_str().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "expected string".into(),
    })?;
    Ok(Some(s.to_string()))
}

// ---------------------------------------------------------------------------
// Color parsing / conversions

fn parse_hex_color(s: &str) -> Option<[f32; 4]> {
    let [r, g, b, a] = parse_hex_color_u8(s)?;
    Some([
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ])
}

fn parse_hex_color_u8(s: &str) -> Option<[u8; 4]> {
    let s = s.strip_prefix('#')?;
    let (r, g, b, a) = match s.len() {
        6 => (
            u8::from_str_radix(&s[0..2], 16).ok()?,
            u8::from_str_radix(&s[2..4], 16).ok()?,
            u8::from_str_radix(&s[4..6], 16).ok()?,
            255,
        ),
        8 => (
            u8::from_str_radix(&s[0..2], 16).ok()?,
            u8::from_str_radix(&s[2..4], 16).ok()?,
            u8::from_str_radix(&s[4..6], 16).ok()?,
            u8::from_str_radix(&s[6..8], 16).ok()?,
        ),
        _ => return None,
    };
    Some([r, g, b, a])
}

pub(super) fn srgb_to_linear_rgba(c: [f32; 4]) -> [f32; 4] {
    fn ch(v: f32) -> f32 {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    [ch(c[0]), ch(c[1]), ch(c[2]), c[3]]
}

pub(super) fn color_to_premul_u8(c: [f32; 4]) -> [u8; 4] {
    let a = c[3].clamp(0.0, 1.0);
    [
        (c[0] * a * 255.0).round() as u8,
        (c[1] * a * 255.0).round() as u8,
        (c[2] * a * 255.0).round() as u8,
        (a * 255.0).round() as u8,
    ]
}

pub(super) fn rgba8_to_color(c: [u8; 4]) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c[0], c[1], c[2], c[3])
}

pub(super) fn tint_alpha_color(c: [u8; 4], alpha_mul: f32) -> tiny_skia::Color {
    let a = ((c[3] as f32) * alpha_mul.clamp(0.0, 1.0)).round() as u8;
    tiny_skia::Color::from_rgba8(c[0], c[1], c[2], a)
}

// ---------------------------------------------------------------------------
// Canvas / raster bridging

/// Build a fresh padded canvas matching the eval ctx.
pub(super) fn make_canvas(ctx: &EvalCtx<'_>) -> Canvas {
    Canvas::new_padded(ctx.canvas.tile_size, ctx.canvas.tile_size, ctx.canvas.pad)
}

/// Consume a freshly-painted [`Canvas`] into a zero-copy [`RasterBuf`].
///
/// Uses `Pixmap::take` so the inner pixel `Vec<u8>` flows straight into
/// the graph layer without `to_vec`. Saves a ~1.3 MB memcpy per paint
/// node on a 564×564 padded canvas.
pub(super) fn canvas_into_raster(canvas: Canvas) -> RasterBuf {
    let pixmap = canvas.into_pixmap();
    let (w, h) = (pixmap.width(), pixmap.height());
    RasterBuf {
        width: w,
        height: h,
        pixels: pixmap.take(),
    }
}

/// Padded transparent raster, used when a paint node has no features
/// to draw (still returns a sized buffer so downstream blends work).
pub(super) fn empty_raster(ctx: &EvalCtx<'_>) -> PortValue {
    let size = ctx.canvas.padded_size();
    PortValue::Raster(Arc::new(RasterBuf::new(size, size)))
}

pub(super) fn core_tile(ctx: &EvalCtx<'_>) -> CoreTileId {
    CoreTileId::new(ctx.tile.z, ctx.tile.x, ctx.tile.y)
}

// ---------------------------------------------------------------------------
// PortValue downcasting

pub(super) fn features_value(
    extent: u32,
    polygons: Vec<ezu_features::Polygon>,
    lines: Vec<Vec<(i32, i32)>>,
) -> PortValue {
    let payload = FilteredFeatures {
        extent,
        polygons,
        lines,
    };
    PortValue::Features(Arc::new(payload) as Arc<dyn Any + Send + Sync>)
}

pub(super) fn downcast_features(v: &PortValue) -> Result<Arc<FilteredFeatures>, EvalError> {
    let PortValue::Features(o) = v else {
        return Err(EvalError::Other("expected Features".into()));
    };
    o.clone()
        .downcast::<FilteredFeatures>()
        .map_err(|_| EvalError::Other("features payload type mismatch".into()))
}

pub(super) fn downcast_brush(v: &PortValue) -> Result<Arc<BrushPayload>, EvalError> {
    let PortValue::Brush(o) = v else {
        return Err(EvalError::Other("expected Brush".into()));
    };
    o.clone()
        .downcast::<BrushPayload>()
        .map_err(|_| EvalError::Other("brush payload type mismatch".into()))
}
