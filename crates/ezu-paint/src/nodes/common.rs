//! Shared helpers for built-in node implementations.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use ezu_core::TileId as CoreTileId;
use ezu_graph::{EvalCtx, EvalError, FactoryCtx, FactoryError, PortKind, PortValue, RasterBuf};
use ezu_style as spec;
use hokusai::Brush;
use serde_json::Value;

use crate::Canvas;

// ---------------------------------------------------------------------------
// Concrete payload types for type-erased ports.

/// One feature's properties paired with its own geometry. A
/// [`FilteredFeatures`] payload is nothing but an ordered list of these — the
/// single source of truth for a `Features` port.
///
/// `properties` are pre-converted (once, at collection time) into the
/// `maplibre-expr` value form and shared behind an `Arc`, so cloning a group —
/// e.g. propagating it through a geometry transform — is a refcount bump, not
/// a deep copy. Data-driven paint nodes evaluate a per-feature expression
/// against these properties (see [`crate::render::group_expr_context`]).
///
/// Synthetic geometry (e.g. `literal-geometry`) and merge-style geometry
/// transforms (e.g. `convex-hull`) emit a single group with empty properties.
pub struct FeatureGroup {
    pub properties: Arc<BTreeMap<String, maplibre_expr::Value>>,
    pub polygons: Vec<ezu_features::Polygon>,
    pub lines: Vec<Vec<(i32, i32)>>,
    pub points: Vec<(i32, i32)>,
}

impl FeatureGroup {
    /// A group with no per-feature properties, for synthetic geometry sources
    /// and geometry transforms that merge geometry across features.
    pub fn synthetic(
        polygons: Vec<ezu_features::Polygon>,
        lines: Vec<Vec<(i32, i32)>>,
        points: Vec<(i32, i32)>,
    ) -> FeatureGroup {
        FeatureGroup {
            properties: Arc::new(BTreeMap::new()),
            polygons,
            lines,
            points,
        }
    }
}

/// Payload carried on a `Features` port. Produced by `features` and the
/// synthetic geometry sources; consumed by every paint and geometry node.
///
/// `groups` is the single source of truth: an ordered list of per-feature
/// [`FeatureGroup`]s. Consumers that want the old flat view — the
/// concatenation of every group's geometry, in group order — walk it via the
/// [`points`](Self::points) / [`lines`](Self::lines) /
/// [`polygons`](Self::polygons) iterators (or collect locally where a
/// contiguous `Vec` is genuinely required).
pub struct FilteredFeatures {
    pub extent: u32,
    pub groups: Vec<FeatureGroup>,
}

impl FilteredFeatures {
    /// Every group's points, in group order (= the old flat concatenation).
    pub fn points(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        self.groups.iter().flat_map(|g| g.points.iter().copied())
    }

    /// Every group's polylines, in group order.
    pub fn lines(&self) -> impl Iterator<Item = &Vec<(i32, i32)>> + '_ {
        self.groups.iter().flat_map(|g| g.lines.iter())
    }

    /// Every group's polygons, in group order.
    pub fn polygons(&self) -> impl Iterator<Item = &ezu_features::Polygon> + '_ {
        self.groups.iter().flat_map(|g| g.polygons.iter())
    }

    /// Whether any group carries at least one point.
    pub fn has_points(&self) -> bool {
        self.groups.iter().any(|g| !g.points.is_empty())
    }

    /// Whether any group carries at least one polyline.
    pub fn has_lines(&self) -> bool {
        self.groups.iter().any(|g| !g.lines.is_empty())
    }

    /// Whether any group carries at least one polygon.
    pub fn has_polygons(&self) -> bool {
        self.groups.iter().any(|g| !g.polygons.is_empty())
    }
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

pub(super) fn read_string_or(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    ctx: &FactoryCtx<'_>,
    default: &str,
) -> Result<String, FactoryError> {
    if !fields.contains_key(name) {
        return Ok(default.to_string());
    }
    let v = resolve_field(fields, name, ctx)?;
    let s = v.as_str().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "expected string".into(),
    })?;
    Ok(s.to_string())
}

/// How to read samples outside the source raster's `[0, w) x [0, h)`
/// extent. The upstream `required_pad` should normally keep us inside,
/// so this only kicks in for extreme amplitudes or at the very edge of
/// the world (zoom 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BoundaryMode {
    /// Clamp to the nearest edge pixel.
    Clamp,
    /// Return transparent black for out-of-bounds samples.
    Transparent,
    /// Reflect at edges so the pattern reads `...|abc|cba|abc|...`.
    Mirror,
}

pub(super) fn read_boundary(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    default: BoundaryMode,
) -> Result<BoundaryMode, FactoryError> {
    let Some(v) = fields.get(name) else {
        return Ok(default);
    };
    let s = v.as_str().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "expected string".into(),
    })?;
    match s {
        "clamp" => Ok(BoundaryMode::Clamp),
        "transparent" => Ok(BoundaryMode::Transparent),
        "mirror" => Ok(BoundaryMode::Mirror),
        _ => Err(FactoryError::BadField {
            field: name.into(),
            msg: format!("unknown boundary `{s}`, expected clamp/transparent/mirror"),
        }),
    }
}

/// Resolve a possibly out-of-range integer pixel coordinate into a
/// valid `[0, dim)` index, or `None` for `Transparent` out-of-range.
#[inline]
fn wrap_index(i: i64, dim: u32, mode: BoundaryMode) -> Option<u32> {
    let d = dim as i64;
    if d == 0 {
        return None;
    }
    if i >= 0 && i < d {
        return Some(i as u32);
    }
    match mode {
        BoundaryMode::Clamp => Some(i.clamp(0, d - 1) as u32),
        BoundaryMode::Transparent => None,
        BoundaryMode::Mirror => {
            // Period 2*(d-1); reflect.
            if d == 1 {
                return Some(0);
            }
            let period = 2 * (d - 1);
            let mut k = i % period;
            if k < 0 {
                k += period;
            }
            if k >= d {
                k = period - k;
            }
            Some(k as u32)
        }
    }
}

#[inline]
fn read_pixel_or(src: &RasterBuf, ix: i64, iy: i64, mode: BoundaryMode) -> [f32; 4] {
    let Some(x) = wrap_index(ix, src.width, mode) else {
        return [0.0; 4];
    };
    let Some(y) = wrap_index(iy, src.height, mode) else {
        return [0.0; 4];
    };
    let p = src.pixel(x, y);
    [p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32]
}

/// Bilinear sample of a premultiplied RGBA8 raster at floating-point
/// pixel coordinates `(x, y)`. Linear blending of premultiplied values
/// is the correct path — avoids halos near transparent edges.
pub(super) fn sample_bilinear(src: &RasterBuf, x: f64, y: f64, mode: BoundaryMode) -> [u8; 4] {
    let fx = x.floor();
    let fy = y.floor();
    let tx = (x - fx) as f32;
    let ty = (y - fy) as f32;
    let ix = fx as i64;
    let iy = fy as i64;
    let p00 = read_pixel_or(src, ix, iy, mode);
    let p10 = read_pixel_or(src, ix + 1, iy, mode);
    let p01 = read_pixel_or(src, ix, iy + 1, mode);
    let p11 = read_pixel_or(src, ix + 1, iy + 1, mode);
    let mut out = [0u8; 4];
    for c in 0..4 {
        let a = p00[c] + (p10[c] - p00[c]) * tx;
        let b = p01[c] + (p11[c] - p01[c]) * tx;
        let v = a + (b - a) * ty;
        out[c] = v.round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// Coordinate anchor for procedural sources (gradients, noise, etc.).
/// `Tile` = positions are fractions of the current tile (constant
/// output per canvas size). `World` = positions are fractions of the
/// full Mercator world at z=0 (output depends on tile id, so the
/// pattern stays continuous across tile boundaries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Anchor {
    Tile,
    World,
}

pub(super) fn read_anchor(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    ctx: &FactoryCtx<'_>,
) -> Result<Anchor, FactoryError> {
    let s = read_string_or(fields, name, ctx, "tile")?;
    match s.as_str() {
        "tile" => Ok(Anchor::Tile),
        "world" => Ok(Anchor::World),
        _ => Err(FactoryError::BadField {
            field: name.into(),
            msg: format!("expected `tile` or `world`, got `{s}`"),
        }),
    }
}

pub(super) fn read_xy(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    ctx: &FactoryCtx<'_>,
    default: [f32; 2],
) -> Result<[f32; 2], FactoryError> {
    if !fields.contains_key(name) {
        return Ok(default);
    }
    let v = resolve_field(fields, name, ctx)?;
    let arr = v.as_array().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "expected [x, y] array".into(),
    })?;
    if arr.len() != 2 {
        return Err(FactoryError::BadField {
            field: name.into(),
            msg: format!("expected exactly 2 numbers, got {}", arr.len()),
        });
    }
    let x = arr[0].as_f64().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "x must be number".into(),
    })? as f32;
    let y = arr[1].as_f64().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "y must be number".into(),
    })? as f32;
    Ok([x, y])
}

/// Parse a gradient `stops` field: `[[t, "#hex"], ...]` with t in [0,1]
/// (non-decreasing) and color as an `#rrggbb[aa]` string. Requires at
/// least two stops.
pub(super) fn read_stops(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    _ctx: &FactoryCtx<'_>,
) -> Result<Vec<(f32, [f32; 4])>, FactoryError> {
    let v = fields
        .get(name)
        .ok_or_else(|| FactoryError::MissingField(name.to_string()))?;
    let arr = v.as_array().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "expected array of [t, color] pairs".into(),
    })?;
    if arr.len() < 2 {
        return Err(FactoryError::BadField {
            field: name.into(),
            msg: "gradient needs at least 2 stops".into(),
        });
    }
    let mut out = Vec::with_capacity(arr.len());
    let mut prev_t: Option<f32> = None;
    for (i, pt) in arr.iter().enumerate() {
        let pair = pt.as_array().ok_or_else(|| FactoryError::BadField {
            field: name.into(),
            msg: format!("entry {i}: expected [t, color] pair"),
        })?;
        if pair.len() != 2 {
            return Err(FactoryError::BadField {
                field: name.into(),
                msg: format!("entry {i}: expected exactly 2 entries"),
            });
        }
        let t = pair[0].as_f64().ok_or_else(|| FactoryError::BadField {
            field: name.into(),
            msg: format!("entry {i}: t must be number"),
        })? as f32;
        let s = pair[1].as_str().ok_or_else(|| FactoryError::BadField {
            field: name.into(),
            msg: format!("entry {i}: color must be hex string"),
        })?;
        let color = parse_hex_color(s).ok_or_else(|| FactoryError::BadField {
            field: name.into(),
            msg: format!("entry {i}: bad color `{s}`"),
        })?;
        if let Some(p) = prev_t {
            if t < p {
                return Err(FactoryError::BadField {
                    field: name.into(),
                    msg: format!("entry {i}: t must be non-decreasing"),
                });
            }
        }
        prev_t = Some(t);
        out.push((t, color));
    }
    Ok(out)
}

/// Read an optional colour-interpolation `space` field (`"rgb"` default,
/// plus `"hsl"` / `"hsv"` / `"hcl"` / `"lab"`). Used by `color-ramp` and the
/// gradient ops so a stop table can interpolate hue on the shortest path.
pub(super) fn read_space(
    fields: &serde_json::Map<String, Value>,
) -> Result<crate::color_interp::InterpSpace, FactoryError> {
    match fields.get("space") {
        None | Some(Value::Null) => Ok(crate::color_interp::InterpSpace::default()),
        Some(v) => {
            let s = v.as_str().ok_or_else(|| FactoryError::BadField {
                field: "space".into(),
                msg: "expected a string".into(),
            })?;
            crate::color_interp::InterpSpace::parse(s).ok_or_else(|| FactoryError::BadField {
                field: "space".into(),
                msg: format!("unknown colour space `{s}` (rgb|hsl|hsv|hcl|lab)"),
            })
        }
    }
}

/// Linearly interpolate gradient stops at parameter `t`, in colour `space`.
/// Stops must be non-empty and sorted by ascending `t`. Out-of-range `t`
/// clamps to the endpoint colors.
pub(super) fn sample_stops(
    stops: &[(f32, [f32; 4])],
    t: f32,
    space: crate::color_interp::InterpSpace,
) -> [f32; 4] {
    if stops.is_empty() {
        return [0.0; 4];
    }
    if t <= stops[0].0 {
        return stops[0].1;
    }
    let last = stops.last().expect("stops is non-empty (guarded above)");
    if t >= last.0 {
        return last.1;
    }
    for w in stops.windows(2) {
        if t >= w[0].0 && t <= w[1].0 {
            let d = w[1].0 - w[0].0;
            if d < 1e-6 {
                return w[1].1;
            }
            let f = (t - w[0].0) / d;
            return crate::color_interp::interpolate(w[0].1, w[1].1, f, space);
        }
    }
    last.1
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

use ezu_style::parse_hex_color;

/// Straight (non-premultiplied) `[0, 1]` components → `[0, 255]`.
pub(super) fn color_f32_to_u8(c: [f32; 4]) -> [u8; 4] {
    [
        (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[3].clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
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
pub(super) fn make_canvas(ctx: &EvalCtx<'_>) -> Result<Canvas, EvalError> {
    Canvas::new_padded(ctx.canvas.tile_size, ctx.canvas.tile_size, ctx.canvas.pad).ok_or_else(
        || {
            EvalError::Other(format!(
                "canvas allocation failed for tile-size={} pad={}",
                ctx.canvas.tile_size, ctx.canvas.pad
            ))
        },
    )
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

pub(super) fn features_value(extent: u32, groups: Vec<FeatureGroup>) -> PortValue {
    let payload = FilteredFeatures { extent, groups };
    PortValue::Features(Arc::new(payload) as Arc<dyn Any + Send + Sync>)
}

/// Accepts list for ports that take either a canvas `Raster` or a
/// native-sized `Sprite`. Filter ops that don't care about the
/// canvas-alignment of their input (e.g. `blur`, `hsl`) use this.
pub(super) const ACCEPTS_RASTER_OR_SPRITE: &[PortKind] = &[PortKind::Raster, PortKind::Sprite];

/// Extract an `Arc<RasterBuf>` from a `PortValue` that is either
/// `Raster` or `Sprite`, returning which variant it came from so the
/// caller can re-wrap the result.
pub(super) fn unwrap_raster_or_sprite(
    v: &PortValue,
    port_name: &str,
) -> Result<(Arc<RasterBuf>, RasterKind), EvalError> {
    match v {
        PortValue::Raster(r) => Ok((r.clone(), RasterKind::Raster)),
        PortValue::Sprite(s) => Ok((s.clone(), RasterKind::Sprite)),
        _ => Err(EvalError::MissingInput(port_name.into())),
    }
}

/// Re-wrap a `RasterBuf` into the same `PortValue` variant it came
/// from — preserves the canvas-vs-native distinction through a
/// pass-through filter node.
pub(super) fn wrap_raster_like(buf: Arc<RasterBuf>, kind: RasterKind) -> PortValue {
    match kind {
        RasterKind::Raster => PortValue::Raster(buf),
        RasterKind::Sprite => PortValue::Sprite(buf),
    }
}

/// Which raster-flavoured [`PortValue`] variant a value originated
/// from. Carried through pass-through filter ops so the output port
/// kind mirrors the input.
#[derive(Debug, Clone, Copy)]
pub(super) enum RasterKind {
    Raster,
    Sprite,
}

/// Resolve a polymorphic Raster/Sprite output kind from the first
/// connected input. Used by `Node::output` for pass-through filter
/// nodes (`blur`, `hsl`, …). Falls back to `Raster` if the input is
/// unconnected (which is a build error anyway for required ports).
pub(super) fn raster_or_sprite_output(input_kinds: &[Option<PortKind>]) -> PortKind {
    match input_kinds.first().and_then(|k| *k) {
        Some(PortKind::Sprite) => PortKind::Sprite,
        _ => PortKind::Raster,
    }
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
