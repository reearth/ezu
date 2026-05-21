//! Node implementations for the graph evaluator.
//!
//! Built-in op set:
//! - Sources / utility (no MVT): `solid`, `mask-solid`, `mask-circle`,
//!   `mask-blur`, `fill-with-mask`, `blend`
//! - MVT-driven: `mvt-source`, `fill-solid`, `fill-dabs`, `line`,
//!   `brush-file`
//!
//! MVT-driven nodes downcast `EvalCtx::tile_data` to
//! `Arc<ezu_mvt::DecodedTile>`. The host (e.g. the `tokyo` example)
//! fetches and decodes the tile and passes it via
//! [`Evaluator::render_with_tile_data`].

use std::any::Any;
use std::sync::Arc;

use ezu_core::TileId as CoreTileId;
use ezu_graph::{
    schema_frag, take_input_ref, Asset, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, MaskBuf, Node, NodeFactory, NodeRegistry, PortKind, PortSpec, PortValue,
    RasterBuf, ScalarValue,
};
use ezu_mvt::DecodedTile;
use ezu_style as spec;
use hokusai::color::RgbaF32;
use hokusai::Brush;
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::{
    paint_lines, paint_polygons, paint_polygons_dabs, render::collect_lines,
    render::collect_polygons, Canvas, DabFillStyle, LineStrokeStyle, WatercolorStyle,
};

/// Build a registry with all built-in ops registered.
pub fn default_registry() -> NodeRegistry {
    let mut r = NodeRegistry::new();
    // raster / mask utility
    r.register("solid", SolidFactory);
    r.register("mask-solid", MaskSolidFactory);
    r.register("mask-circle", MaskCircleFactory);
    r.register("mask-blur", MaskBlurFactory);
    r.register("fill-with-mask", FillWithMaskFactory);
    r.register("blend", BlendFactory);
    // mvt-driven
    r.register("mvt-source", MvtSourceFactory);
    r.register("fill-solid", FillSolidFactory);
    r.register("fill-dabs", FillDabsFactory);
    r.register("line", LineFactory);
    r.register("brush-file", BrushFileFactory);
    r
}

// ---------------------------------------------------------------------------
// Concrete payload types for type-erased ports.

/// Payload carried on a `Features` port. Produced by `mvt-source`;
/// consumed by `fill-solid`, `fill-dabs`, `line`.
pub struct FilteredFeatures {
    pub extent: u32,
    pub polygons: Vec<ezu_mvt::Polygon>,
    pub lines: Vec<Vec<(i32, i32)>>,
}

/// Payload carried on a `Brush` port. Wraps a hokusai brush.
pub type BrushPayload = Brush;

// ---------------------------------------------------------------------------
// Param-reading helpers

fn resolve_field(
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

fn read_color(
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

fn read_number(
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

fn read_number_or(
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

fn parse_hex_color(s: &str) -> Option<[f32; 4]> {
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
    Some([
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ])
}

// ---------------------------------------------------------------------------
// solid: () -> Raster

struct SolidNode {
    color: [f32; 4],
}

impl Node for SolidNode {
    fn op_name(&self) -> &'static str {
        "solid"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self) -> PortKind {
        PortKind::Raster
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        _: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let size = ctx.canvas.padded_size();
        let rgba = color_to_premul_u8(self.color);
        Ok(PortValue::Raster(Arc::new(RasterBuf::filled(
            size, size, rgba,
        ))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"solid");
        for c in self.color {
            h.update(&c.to_le_bytes());
        }
    }
}

struct SolidFactory;
impl NodeFactory for SolidFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let color = read_color(fields, "color", ctx)?;
        Ok(BuiltNode {
            node: Box::new(SolidNode { color }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Solid-color raster source filling the entire canvas.",
            "properties": { "color": schema_frag::color() },
            "required": ["color"],
        })
    }
}

// ---------------------------------------------------------------------------
// mask-solid: () -> Mask, uniform value

struct MaskSolidNode {
    value: f32,
}
impl Node for MaskSolidNode {
    fn op_name(&self) -> &'static str {
        "mask-solid"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self) -> PortKind {
        PortKind::Mask
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        _: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let size = ctx.canvas.padded_size();
        Ok(PortValue::Mask(Arc::new(MaskBuf::filled(
            size, size, self.value,
        ))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"mask-solid");
        h.update(&self.value.to_le_bytes());
    }
}
struct MaskSolidFactory;
impl NodeFactory for MaskSolidFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let value = read_number(fields, "value", ctx)? as f32;
        Ok(BuiltNode {
            node: Box::new(MaskSolidNode { value }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Uniform-value mask source.",
            "properties": { "value": schema_frag::unit_number() },
            "required": ["value"],
        })
    }
}

// ---------------------------------------------------------------------------
// mask-circle: () -> Mask. Centered disk, radius given in fraction of
// tile_size (0..1). Useful for testing without MVT.

struct MaskCircleNode {
    radius_frac: f32,
    hardness: f32,
}
impl Node for MaskCircleNode {
    fn op_name(&self) -> &'static str {
        "mask-circle"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self) -> PortKind {
        PortKind::Mask
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        _: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let size = ctx.canvas.padded_size();
        let mut m = MaskBuf::new(size, size);
        let cx = size as f32 * 0.5;
        let cy = size as f32 * 0.5;
        let r = ctx.canvas.tile_size as f32 * self.radius_frac;
        let h = self.hardness.clamp(0.0, 0.999);
        let inner = r * h;
        for y in 0..size {
            for x in 0..size {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let d = (dx * dx + dy * dy).sqrt();
                let v = if d <= inner {
                    1.0
                } else if d >= r {
                    0.0
                } else {
                    1.0 - (d - inner) / (r - inner)
                };
                m.pixels[(y * size + x) as usize] = v;
            }
        }
        Ok(PortValue::Mask(Arc::new(m)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"mask-circle");
        h.update(&self.radius_frac.to_le_bytes());
        h.update(&self.hardness.to_le_bytes());
    }
}
struct MaskCircleFactory;
impl NodeFactory for MaskCircleFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let radius_frac = read_number(fields, "radius-frac", ctx)? as f32;
        let hardness = read_number_or(fields, "hardness", ctx, 1.0)? as f32;
        Ok(BuiltNode {
            node: Box::new(MaskCircleNode {
                radius_frac,
                hardness,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Centered disk mask. Radius is a fraction of `tile-size`.",
            "properties": {
                "radius-frac": schema_frag::unit_number(),
                "hardness": schema_frag::unit_number(),
            },
            "required": ["radius-frac"],
        })
    }
}

// ---------------------------------------------------------------------------
// mask-blur: Mask -> Mask, separable box-approximated gaussian.

struct MaskBlurNode {
    sigma: f32,
}
impl Node for MaskBlurNode {
    fn op_name(&self) -> &'static str {
        "mask-blur"
    }
    fn inputs(&self) -> &[PortSpec] {
        // Static slice via `pub const`. Use a leak-once trick.
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "input",
            kind: PortKind::Mask,
            optional: false,
        }];
        SPECS
    }
    fn output(&self) -> PortKind {
        PortKind::Mask
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + (3.0 * self.sigma).ceil() as u32
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let src = inputs[0]
            .as_ref()
            .and_then(PortValue::as_mask)
            .ok_or_else(|| EvalError::MissingInput("input".into()))?
            .clone();
        let out = gaussian_blur_mask(&src, self.sigma);
        Ok(PortValue::Mask(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"mask-blur");
        h.update(&self.sigma.to_le_bytes());
    }
}
struct MaskBlurFactory;
impl NodeFactory for MaskBlurFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let sigma = read_number(fields, "sigma", ctx)? as f32;
        Ok(BuiltNode {
            node: Box::new(MaskBlurNode { sigma }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Separable Gaussian blur on a mask. Grows upstream pad by 3σ.",
            "properties": {
                "input": schema_frag::node_ref(),
                "sigma": schema_frag::px_number(),
            },
            "required": ["input", "sigma"],
        })
    }
}

fn gaussian_blur_mask(src: &MaskBuf, sigma: f32) -> MaskBuf {
    if sigma <= 0.0 {
        return src.clone();
    }
    let kernel = gaussian_kernel(sigma);
    let kh = (kernel.len() / 2) as i32;
    let w = src.width as i32;
    let h = src.height as i32;
    // Horizontal pass.
    let mut tmp = vec![0.0f32; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0.0;
            for (i, k) in kernel.iter().enumerate() {
                let sx = (x + i as i32 - kh).clamp(0, w - 1);
                sum += k * src.pixels[(y * w + sx) as usize];
            }
            tmp[(y * w + x) as usize] = sum;
        }
    }
    // Vertical pass.
    let mut out = MaskBuf::new(src.width, src.height);
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0.0;
            for (i, k) in kernel.iter().enumerate() {
                let sy = (y + i as i32 - kh).clamp(0, h - 1);
                sum += k * tmp[(sy * w + x) as usize];
            }
            out.pixels[(y * w + x) as usize] = sum;
        }
    }
    out
}

fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let radius = (3.0 * sigma).ceil() as i32;
    let len = (2 * radius + 1) as usize;
    let mut k = Vec::with_capacity(len);
    let two_s2 = 2.0 * sigma * sigma;
    let mut sum = 0.0;
    for i in -radius..=radius {
        let v = (-(i as f32 * i as f32) / two_s2).exp();
        k.push(v);
        sum += v;
    }
    for v in &mut k {
        *v /= sum;
    }
    k
}

// ---------------------------------------------------------------------------
// fill-with-mask: Mask + scalar Color -> Raster

struct FillWithMaskNode {
    color: [f32; 4],
}
impl Node for FillWithMaskNode {
    fn op_name(&self) -> &'static str {
        "fill-with-mask"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "mask",
            kind: PortKind::Mask,
            optional: false,
        }];
        SPECS
    }
    fn output(&self) -> PortKind {
        PortKind::Raster
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let mask = inputs[0]
            .as_ref()
            .and_then(PortValue::as_mask)
            .ok_or_else(|| EvalError::MissingInput("mask".into()))?;
        let mut out = RasterBuf::new(mask.width, mask.height);
        let [r, g, b, a] = self.color;
        for i in 0..mask.pixels.len() {
            let m = mask.pixels[i].clamp(0.0, 1.0);
            let alpha = a * m;
            let pr = r * alpha;
            let pg = g * alpha;
            let pb = b * alpha;
            let o = i * 4;
            out.pixels[o] = (pr * 255.0).round() as u8;
            out.pixels[o + 1] = (pg * 255.0).round() as u8;
            out.pixels[o + 2] = (pb * 255.0).round() as u8;
            out.pixels[o + 3] = (alpha * 255.0).round() as u8;
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"fill-with-mask");
        for c in self.color {
            h.update(&c.to_le_bytes());
        }
    }
}
struct FillWithMaskFactory;
impl NodeFactory for FillWithMaskFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let mask = take_input_ref(fields, "mask")?;
        let color = read_color(fields, "color", ctx)?;
        Ok(BuiltNode {
            node: Box::new(FillWithMaskNode { color }),
            connections: vec![Connection {
                port: "mask".into(),
                src: mask,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Tint a mask with a solid color, producing a premultiplied raster.",
            "properties": {
                "mask": schema_frag::node_ref(),
                "color": schema_frag::color(),
            },
            "required": ["mask", "color"],
        })
    }
}

// ---------------------------------------------------------------------------
// blend: Raster base + Raster over -> Raster (alpha-over, sRGB premul)

struct BlendNode {
    opacity: f32,
}
impl Node for BlendNode {
    fn op_name(&self) -> &'static str {
        "blend"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[
            PortSpec {
                name: "base",
                kind: PortKind::Raster,
                optional: false,
            },
            PortSpec {
                name: "over",
                kind: PortKind::Raster,
                optional: false,
            },
        ];
        SPECS
    }
    fn output(&self) -> PortKind {
        PortKind::Raster
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let base = inputs[0]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("base".into()))?;
        let over = inputs[1]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("over".into()))?;
        if base.width != over.width || base.height != over.height {
            return Err(EvalError::Other("blend: size mismatch".into()));
        }
        // Premultiplied source-over with optional opacity, integer-only
        // fast path. For a properly premultiplied `over` buffer, scaling
        // its alpha by `op` requires scaling its colors by `op` too —
        // hence the single `op_q` factor applied to all four channels.
        // Output stays in [0, 255] by the premul invariant, so no
        // saturation is needed.
        let mut out = RasterBuf::new(base.width, base.height);
        let op_q = (self.opacity.clamp(0.0, 1.0) * 255.0).round() as u16;
        let bp = &base.pixels;
        let op_buf = &over.pixels;
        let dst = &mut out.pixels;
        for i in (0..bp.len()).step_by(4) {
            let o0 = mul_u8q(op_buf[i], op_q);
            let o1 = mul_u8q(op_buf[i + 1], op_q);
            let o2 = mul_u8q(op_buf[i + 2], op_q);
            let oa = mul_u8q(op_buf[i + 3], op_q);
            let inv = 255u16 - oa as u16;
            dst[i] = o0 + mul_u8q(bp[i], inv);
            dst[i + 1] = o1 + mul_u8q(bp[i + 1], inv);
            dst[i + 2] = o2 + mul_u8q(bp[i + 2], inv);
            dst[i + 3] = oa + mul_u8q(bp[i + 3], inv);
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"blend");
        h.update(&self.opacity.to_le_bytes());
    }
}
struct BlendFactory;
impl NodeFactory for BlendFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let base = take_input_ref(fields, "base")?;
        let over = take_input_ref(fields, "over")?;
        let opacity = read_number_or(fields, "opacity", ctx, 1.0)? as f32;
        Ok(BuiltNode {
            node: Box::new(BlendNode { opacity }),
            connections: vec![
                Connection {
                    port: "base".into(),
                    src: base,
                },
                Connection {
                    port: "over".into(),
                    src: over,
                },
            ],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Source-over composite (premultiplied) of `over` on top of `base`.",
            "properties": {
                "base": schema_frag::node_ref(),
                "over": schema_frag::node_ref(),
                "opacity": schema_frag::unit_number(),
            },
            "required": ["base", "over"],
        })
    }
}

// ---------------------------------------------------------------------------

/// Multiply a u8 channel by a 0..=255 quantized factor with proper
/// rounding: `(c * q + 127) / 255`. Wraps to `u8` (caller must ensure
/// the result fits — true for any premul-correct alpha-over math).
#[inline(always)]
fn mul_u8q(c: u8, q: u16) -> u8 {
    ((c as u16 * q + 127) / 255) as u8
}

fn color_to_premul_u8(c: [f32; 4]) -> [u8; 4] {
    let a = c[3].clamp(0.0, 1.0);
    [
        (c[0] * a * 255.0).round() as u8,
        (c[1] * a * 255.0).round() as u8,
        (c[2] * a * 255.0).round() as u8,
        (a * 255.0).round() as u8,
    ]
}

// Silence "unused import" if ScalarValue ends up unused in this module.
#[allow(dead_code)]
fn _scalar_marker(_: ScalarValue) {}

// ===========================================================================
// MVT-driven nodes
// ===========================================================================

/// Consume a freshly-painted [`Canvas`] into a zero-copy [`RasterBuf`].
///
/// Uses `Pixmap::take` so the inner pixel `Vec<u8>` flows straight into
/// the graph layer without `to_vec`. Saves a ~1.3 MB memcpy per paint
/// node on a 564×564 padded canvas.
fn canvas_into_raster(canvas: Canvas) -> RasterBuf {
    let pixmap = canvas.into_pixmap();
    let (w, h) = (pixmap.width(), pixmap.height());
    RasterBuf {
        width: w,
        height: h,
        pixels: pixmap.take(),
    }
}

fn make_canvas(ctx: &EvalCtx<'_>) -> Canvas {
    Canvas::new_padded(ctx.canvas.tile_size, ctx.canvas.tile_size, ctx.canvas.pad)
}

fn empty_raster(ctx: &EvalCtx<'_>) -> PortValue {
    let size = ctx.canvas.padded_size();
    PortValue::Raster(Arc::new(RasterBuf::new(size, size)))
}

fn core_tile(ctx: &EvalCtx<'_>) -> CoreTileId {
    CoreTileId::new(ctx.tile.z, ctx.tile.x, ctx.tile.y)
}

fn read_optional_string(
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
// mvt-source: () -> Features

struct MvtSourceNode {
    source_layer: String,
    filter: Option<ezu_style::FeatureFilter>,
    min_zoom_field: Option<String>,
}

impl Node for MvtSourceNode {
    fn op_name(&self) -> &'static str {
        "mvt-source"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self) -> PortKind {
        PortKind::Features
    }
    fn coord_space(&self) -> ezu_graph::CoordSpace {
        // Features are tile-local (MVT geometry is in [0, extent]).
        ezu_graph::CoordSpace::Tile
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        _inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let z = ctx.tile.z;
        let (extent, polygons, lines) = match ctx.tile_data {
            None => (0u32, vec![], vec![]),
            Some(opaque) => {
                let tile = opaque
                    .clone()
                    .downcast::<DecodedTile>()
                    .map_err(|_| EvalError::Other("tile_data is not Arc<DecodedTile>".into()))?;
                match tile.layer(&self.source_layer) {
                    None => (0u32, vec![], vec![]),
                    Some(layer) => {
                        let polys = collect_polygons(
                            &layer.features,
                            &self.filter,
                            &self.min_zoom_field,
                            z,
                        );
                        let lns = collect_lines(
                            &layer.features,
                            &self.filter,
                            &self.min_zoom_field,
                            z,
                        );
                        (layer.extent, polys, lns)
                    }
                }
            }
        };
        Ok(features_value((extent, polygons, lines)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"mvt-source");
        h.update(self.source_layer.as_bytes());
        if let Some(f) = &self.filter {
            let mut keys: Vec<&String> = f.keys().collect();
            keys.sort();
            for k in keys {
                h.update(k.as_bytes());
                // Lightweight hash of the FilterMatch via Debug; not
                // beautiful but stable enough for cache invalidation.
                h.update(format!("{:?}", f[k]).as_bytes());
            }
        }
        if let Some(s) = &self.min_zoom_field {
            h.update(s.as_bytes());
        }
    }
}

fn features_value(t: (u32, Vec<ezu_mvt::Polygon>, Vec<Vec<(i32, i32)>>)) -> PortValue {
    let payload = FilteredFeatures {
        extent: t.0,
        polygons: t.1,
        lines: t.2,
    };
    PortValue::Features(Arc::new(payload) as Arc<dyn Any + Send + Sync>)
}

struct MvtSourceFactory;
impl NodeFactory for MvtSourceFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let source_layer = fields
            .get("source-layer")
            .and_then(Value::as_str)
            .ok_or_else(|| FactoryError::MissingField("source-layer".into()))?
            .to_string();
        let filter = match fields.get("filter") {
            Some(v) => Some(serde_json::from_value::<ezu_style::FeatureFilter>(v.clone())
                .map_err(|e| FactoryError::BadField {
                    field: "filter".into(),
                    msg: e.to_string(),
                })?),
            None => None,
        };
        let min_zoom_field = read_optional_string(fields, "min-zoom-field")?;
        Ok(BuiltNode {
            node: Box::new(MvtSourceNode {
                source_layer,
                filter,
                min_zoom_field,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Select features from a host-supplied MVT layer.",
            "properties": {
                "source-layer": { "type": "string" },
                "filter": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "Property-value filter; entries are AND-combined."
                },
                "min-zoom-field": { "type": "string" },
            },
            "required": ["source-layer"],
        })
    }
}

fn downcast_features(v: &PortValue) -> Result<Arc<FilteredFeatures>, EvalError> {
    let PortValue::Features(o) = v else {
        return Err(EvalError::Other("expected Features".into()));
    };
    o.clone()
        .downcast::<FilteredFeatures>()
        .map_err(|_| EvalError::Other("features payload type mismatch".into()))
}

fn downcast_brush(v: &PortValue) -> Result<Arc<BrushPayload>, EvalError> {
    let PortValue::Brush(o) = v else {
        return Err(EvalError::Other("expected Brush".into()));
    };
    o.clone()
        .downcast::<BrushPayload>()
        .map_err(|_| EvalError::Other("brush payload type mismatch".into()))
}

// ---------------------------------------------------------------------------
// brush-file: () -> Brush

struct BrushFileNode {
    src: String,
}
impl Node for BrushFileNode {
    fn op_name(&self) -> &'static str {
        "brush-file"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self) -> PortKind {
        PortKind::Brush
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        _: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let asset = ctx.assets.load(&self.src)?;
        let Asset::Brush(b) = asset else {
            return Err(EvalError::Other(format!(
                "asset `{}` is not a brush",
                self.src
            )));
        };
        Ok(PortValue::Brush(b))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"brush-file");
        h.update(self.src.as_bytes());
    }
}
struct BrushFileFactory;
impl NodeFactory for BrushFileFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let raw = fields
            .get("src")
            .and_then(Value::as_str)
            .ok_or_else(|| FactoryError::MissingField("src".into()))?;
        // `@name` -> look up in document `assets` and use that entry's
        // src. Otherwise the literal is passed straight to the loader.
        let src = match spec::FieldRef::classify(raw) {
            spec::FieldRef::Node(name) => {
                let asset = ctx
                    .assets
                    .get(name)
                    .ok_or_else(|| FactoryError::UnknownAsset(name.to_string()))?;
                if asset.kind != spec::AssetKind::Brush {
                    return Err(FactoryError::BadField {
                        field: "src".into(),
                        msg: format!("asset `{name}` is not a brush"),
                    });
                }
                asset.src.clone()
            }
            spec::FieldRef::Literal(s) => s.to_string(),
            spec::FieldRef::Param(_) => {
                return Err(FactoryError::BadField {
                    field: "src".into(),
                    msg: "param refs not allowed for brush src".into(),
                })
            }
        };
        Ok(BuiltNode {
            node: Box::new(BrushFileNode { src }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Brush source. `src` is an `@asset` ref or a literal path/name resolved by the host's AssetLoader.",
            "properties": { "src": schema_frag::asset_ref() },
            "required": ["src"],
        })
    }
}

// ---------------------------------------------------------------------------
// fill-solid: Features -> Raster

struct FillSolidV1Node {
    fill: [u8; 4],
    fill_alpha: f32,
    edge: Option<[u8; 4]>,
    edge_width: f32,
    blur_sigma: f32,
}
impl Node for FillSolidV1Node {
    fn op_name(&self) -> &'static str {
        "fill-solid"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "features",
            kind: PortKind::Features,
            optional: false,
        }];
        SPECS
    }
    fn output(&self) -> PortKind {
        PortKind::Raster
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + (3.0 * self.blur_sigma.max(0.0)).ceil() as u32
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let feats = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("features".into()))?;
        let feats = downcast_features(feats)?;
        if feats.polygons.is_empty() {
            return Ok(empty_raster(ctx));
        }
        let mut canvas = make_canvas(ctx);
        let style = WatercolorStyle {
            fill: tint_alpha_color(self.fill, self.fill_alpha),
            edge: self.edge.map(rgba8_to_color),
            edge_width: self.edge_width,
            blur_sigma: self.blur_sigma,
        };
        paint_polygons(&mut canvas, &feats.polygons, feats.extent, &style);
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"fill-solid");
        h.update(&self.fill);
        h.update(&self.fill_alpha.to_le_bytes());
        if let Some(e) = self.edge {
            h.update(&[1]);
            h.update(&e);
        } else {
            h.update(&[0]);
        }
        h.update(&self.edge_width.to_le_bytes());
        h.update(&self.blur_sigma.to_le_bytes());
    }
}
struct FillSolidFactory;
impl NodeFactory for FillSolidFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let fill = read_color_u8(fields, "fill", ctx)?;
        let fill_alpha = read_number_or(fields, "fill-alpha", ctx, 1.0)? as f32;
        let edge = match fields.get("edge") {
            Some(_) => Some(read_color_u8(fields, "edge", ctx)?),
            None => None,
        };
        let edge_width = read_number_or(fields, "edge-width", ctx, 1.0)? as f32;
        let blur_sigma = read_number_or(fields, "blur-sigma", ctx, 0.0)? as f32;
        Ok(BuiltNode {
            node: Box::new(FillSolidV1Node {
                fill,
                fill_alpha,
                edge,
                edge_width,
                blur_sigma,
            }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Solid polygon fill with optional outline and Gaussian blur.",
            "properties": {
                "features": schema_frag::node_ref(),
                "fill": schema_frag::color(),
                "fill-alpha": schema_frag::unit_number(),
                "edge": schema_frag::color(),
                "edge-width": schema_frag::px_number(),
                "blur-sigma": schema_frag::px_number(),
            },
            "required": ["features", "fill"],
        })
    }
}

// ---------------------------------------------------------------------------
// fill-dabs: Features -> Raster (no brush input; uses hokusai built-in
// scatter painter, parameterised inline. Matches the v0 semantics exactly.)

struct FillDabsV1Node {
    color: [f32; 4],
    opacity: f32,
    radius_px: f32,
    hardness: f32,
    paint: f32,
    spacing_px: f32,
    position_jitter: f32,
    size_jitter: f32,
    opacity_jitter: f32,
    value_jitter: f32,
}
impl Node for FillDabsV1Node {
    fn op_name(&self) -> &'static str {
        "fill-dabs"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "features",
            kind: PortKind::Features,
            optional: false,
        }];
        SPECS
    }
    fn output(&self) -> PortKind {
        PortKind::Raster
    }
    fn coord_space(&self) -> ezu_graph::CoordSpace {
        ezu_graph::CoordSpace::World
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let feats = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("features".into()))?;
        let feats = downcast_features(feats)?;
        if feats.polygons.is_empty() {
            return Ok(empty_raster(ctx));
        }
        let mut canvas = make_canvas(ctx);
        let style = DabFillStyle {
            color: RgbaF32::new(self.color[0], self.color[1], self.color[2], 1.0),
            opacity: self.opacity,
            radius_px: self.radius_px,
            hardness: self.hardness,
            paint: self.paint,
            spacing_px: self.spacing_px,
            position_jitter: self.position_jitter,
            size_jitter: self.size_jitter,
            opacity_jitter: self.opacity_jitter,
            value_jitter: self.value_jitter,
        };
        paint_polygons_dabs(&mut canvas, &feats.polygons, feats.extent, core_tile(ctx), &style);
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"fill-dabs");
        for c in self.color {
            h.update(&c.to_le_bytes());
        }
        for v in [
            self.opacity,
            self.radius_px,
            self.hardness,
            self.paint,
            self.spacing_px,
            self.position_jitter,
            self.size_jitter,
            self.opacity_jitter,
            self.value_jitter,
        ] {
            h.update(&v.to_le_bytes());
        }
    }
}
struct FillDabsFactory;
impl NodeFactory for FillDabsFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let color_srgb = read_color(fields, "color", ctx)?;
        let color = srgb_to_linear_rgba(color_srgb);
        let opacity = read_number(fields, "opacity", ctx)? as f32;
        let radius_px = read_number(fields, "radius-px", ctx)? as f32;
        let hardness = read_number_or(fields, "hardness", ctx, 0.5)? as f32;
        let paint = read_number_or(fields, "paint", ctx, 1.0)? as f32;
        let spacing_px = read_number(fields, "spacing-px", ctx)? as f32;
        let position_jitter = read_number_or(fields, "position-jitter", ctx, 0.9)? as f32;
        let size_jitter = read_number_or(fields, "size-jitter", ctx, 0.0)? as f32;
        let opacity_jitter = read_number_or(fields, "opacity-jitter", ctx, 0.0)? as f32;
        let value_jitter = read_number_or(fields, "value-jitter", ctx, 0.0)? as f32;
        Ok(BuiltNode {
            node: Box::new(FillDabsV1Node {
                color,
                opacity,
                radius_px,
                hardness,
                paint,
                spacing_px,
                position_jitter,
                size_jitter,
                opacity_jitter,
                value_jitter,
            }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Watercolor scatter-dab fill with world-deterministic jitter (seamless across tiles).",
            "properties": {
                "features": schema_frag::node_ref(),
                "color": schema_frag::color(),
                "opacity": schema_frag::unit_number(),
                "radius-px": schema_frag::px_number(),
                "hardness": schema_frag::unit_number(),
                "paint": schema_frag::unit_number(),
                "spacing-px": schema_frag::px_number(),
                "position-jitter": schema_frag::unit_number(),
                "size-jitter": schema_frag::unit_number(),
                "opacity-jitter": schema_frag::unit_number(),
                "value-jitter": schema_frag::unit_number(),
            },
            "required": ["features", "color", "opacity", "radius-px", "spacing-px"],
        })
    }
}

// ---------------------------------------------------------------------------
// line: Features + Brush -> Raster

struct LineV1Node {
    color: [f32; 3],
    pressure_base: f32,
    pressure_jitter: f32,
    dtime: f32,
    radius_px: Option<f32>,
    opacity: Option<f32>,
}
impl Node for LineV1Node {
    fn op_name(&self) -> &'static str {
        "line"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[
            PortSpec {
                name: "features",
                kind: PortKind::Features,
                optional: false,
            },
            PortSpec {
                name: "brush",
                kind: PortKind::Brush,
                optional: false,
            },
        ];
        SPECS
    }
    fn output(&self) -> PortKind {
        PortKind::Raster
    }
    fn coord_space(&self) -> ezu_graph::CoordSpace {
        ezu_graph::CoordSpace::World
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let feats = downcast_features(
            inputs[0]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("features".into()))?,
        )?;
        let brush_arc = downcast_brush(
            inputs[1]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("brush".into()))?,
        )?;
        if feats.lines.is_empty() {
            return Ok(empty_raster(ctx));
        }
        let mut canvas = make_canvas(ctx);
        // Clone brush and apply optional radius / opacity overrides.
        let mut brush: Brush = (*brush_arc).clone();
        if let Some(r) = self.radius_px {
            brush.get_mut(hokusai::BrushSetting::Radius).base_value = r.max(0.05).ln();
        }
        if let Some(o) = self.opacity {
            brush.get_mut(hokusai::BrushSetting::Opaque).base_value = o.clamp(0.0, 1.0);
        }
        let style = LineStrokeStyle {
            color: self.color,
            pressure_base: self.pressure_base,
            pressure_jitter: self.pressure_jitter,
            dtime: self.dtime,
        };
        paint_lines(&mut canvas, &feats.lines, feats.extent, core_tile(ctx), &brush, &style);
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"line");
        for c in self.color {
            h.update(&c.to_le_bytes());
        }
        for v in [self.pressure_base, self.pressure_jitter, self.dtime] {
            h.update(&v.to_le_bytes());
        }
        if let Some(r) = self.radius_px {
            h.update(&[1]);
            h.update(&r.to_le_bytes());
        } else {
            h.update(&[0]);
        }
        if let Some(o) = self.opacity {
            h.update(&[1]);
            h.update(&o.to_le_bytes());
        } else {
            h.update(&[0]);
        }
    }
}
struct LineFactory;
impl NodeFactory for LineFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let brush = take_input_ref(fields, "brush")?;
        let color_srgb = read_color(fields, "color", ctx)?;
        let lin = srgb_to_linear_rgba(color_srgb);
        let color = [lin[0], lin[1], lin[2]];
        let pressure_base = read_number_or(fields, "pressure-base", ctx, 0.7)? as f32;
        let pressure_jitter = read_number_or(fields, "pressure-jitter", ctx, 0.2)? as f32;
        let dtime = read_number_or(fields, "dtime", ctx, 0.02)? as f32;
        let radius_px = if fields.contains_key("radius-px") {
            Some(read_number(fields, "radius-px", ctx)? as f32)
        } else {
            None
        };
        let opacity = if fields.contains_key("opacity") {
            Some(read_number(fields, "opacity", ctx)? as f32)
        } else {
            None
        };
        Ok(BuiltNode {
            node: Box::new(LineV1Node {
                color,
                pressure_base,
                pressure_jitter,
                dtime,
                radius_px,
                opacity,
            }),
            connections: vec![
                Connection {
                    port: "features".into(),
                    src: features,
                },
                Connection {
                    port: "brush".into(),
                    src: brush,
                },
            ],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Brush stroke along MVT polylines.",
            "properties": {
                "features": schema_frag::node_ref(),
                "brush": schema_frag::node_ref(),
                "color": schema_frag::color(),
                "radius-px": schema_frag::px_number(),
                "opacity": schema_frag::unit_number(),
                "pressure-base": schema_frag::unit_number(),
                "pressure-jitter": schema_frag::unit_number(),
                "dtime": { "type": "number", "minimum": 0.0 },
            },
            "required": ["features", "brush", "color"],
        })
    }
}

// ---------------------------------------------------------------------------
// Color helpers shared with MVT-driven nodes.

fn read_color_u8(
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

fn srgb_to_linear_rgba(c: [f32; 4]) -> [f32; 4] {
    fn ch(v: f32) -> f32 {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    [ch(c[0]), ch(c[1]), ch(c[2]), c[3]]
}

fn rgba8_to_color(c: [u8; 4]) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c[0], c[1], c[2], c[3])
}

fn tint_alpha_color(c: [u8; 4], alpha_mul: f32) -> tiny_skia::Color {
    let a = ((c[3] as f32) * alpha_mul.clamp(0.0, 1.0)).round() as u8;
    tiny_skia::Color::from_rgba8(c[0], c[1], c[2], a)
}
