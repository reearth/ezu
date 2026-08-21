//! `circle` — centred disk source. Emits a canvas-sized `Raster`
//! (radius = `radius-frac × tile-size`) by default, or a `Sprite` at
//! `width-px × height-px` when `kind: "sprite"` (radius is then a
//! fraction of `min(width, height)`). Premultiplied RGBA output with
//! optional `hardness` edge falloff.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, In, InReader, Node,
    NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::raster::generator_kind::{parse_generator_kind, GeneratorKind};

struct CircleNode {
    color: In<[f32; 4]>,
    radius_frac: In<f64>,
    hardness: In<f64>,
    out_kind: GeneratorKind,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for CircleNode {
    fn op_name(&self) -> &'static str {
        "circle"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        match self.out_kind {
            GeneratorKind::Raster => PortKind::Raster,
            GeneratorKind::Sprite { .. } => PortKind::Sprite,
        }
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        // Raster mode anchors radius to `tile-size` (so the disk
        // visually scales with the tile geometry). Sprite mode
        // anchors to the shorter sprite side so the disk fits.
        let (out_w, out_h, radius_unit) = match self.out_kind {
            GeneratorKind::Raster => {
                let (pw, ph) = ctx.canvas.padded_dims();
                (pw, ph, ctx.canvas.tile_w as f32)
            }
            GeneratorKind::Sprite { width, height } => (width, height, width.min(height) as f32),
        };
        let color = self.color.get(ctx, inputs)?;
        let radius_frac = self.radius_frac.get(ctx, inputs)? as f32;
        let hardness = self.hardness.get(ctx, inputs)? as f32;
        let mut out = RasterBuf::new(out_w, out_h);
        let cx = out_w as f32 * 0.5;
        let cy = out_h as f32 * 0.5;
        let r = radius_unit * radius_frac;
        let h = hardness.clamp(0.0, 0.999);
        let inner = r * h;
        let [cr, cg, cb, ca] = color;
        for y in 0..out_h {
            for x in 0..out_w {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let d = (dx * dx + dy * dy).sqrt();
                let m = if d <= inner {
                    1.0
                } else if d >= r {
                    0.0
                } else {
                    1.0 - (d - inner) / (r - inner)
                };
                let alpha = ca * m;
                let i = ((y * out_w + x) * 4) as usize;
                out.pixels[i] = (cr * alpha * 255.0).round() as u8;
                out.pixels[i + 1] = (cg * alpha * 255.0).round() as u8;
                out.pixels[i + 2] = (cb * alpha * 255.0).round() as u8;
                out.pixels[i + 3] = (alpha * 255.0).round() as u8;
            }
        }
        let buf = Arc::new(out);
        Ok(match self.out_kind {
            GeneratorKind::Raster => PortValue::Raster(buf),
            GeneratorKind::Sprite { .. } => PortValue::Sprite(buf),
        })
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"circle");
        self.color.param_hash(h);
        self.radius_frac.param_hash(h);
        self.hardness.param_hash(h);
        let (tag, dims) = self.out_kind.hash_tag();
        h.update(&tag);
        if let Some((w, hh)) = dims {
            h.update(&w.to_le_bytes());
            h.update(&hh.to_le_bytes());
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct CircleFactory;
impl NodeFactory for CircleFactory {
    fn op_name(&self) -> &'static str {
        "circle"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let mut r = InReader::new(fields, ctx, 0);
        let color = r.color("color")?;
        let radius_frac = r.number("radius-frac")?;
        let hardness = r.number_or("hardness", 1.0)?;
        let parts = r.finish();
        let out_kind = parse_generator_kind(fields, ctx)?;
        Ok(BuiltNode {
            node: Box::new(CircleNode {
                color,
                radius_frac,
                hardness,
                out_kind,
                ports: parts.ports,
                param_refs: parts.param_refs,
            }),
            connections: parts.connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Centred disk source. `kind: raster` (default) renders at canvas size with radius = `radius-frac × tile-size`. `kind: sprite` renders at `width-px × height-px` with radius = `radius-frac × min(width, height)`.",
            "properties": {
                "color": schema_frag::color(),
                "radius-frac": schema_frag::unit_number(),
                "hardness": schema_frag::unit_number(),
                "kind": { "type": "string", "enum": ["raster", "sprite"], "default": "raster" },
                "width-px": { "type": "integer", "minimum": 1 },
                "height-px": { "type": "integer", "minimum": 1 },
            },
            "required": ["color", "radius-frac"],
        })
    }
}

ezu_graph::submit_node!(CircleFactory);
