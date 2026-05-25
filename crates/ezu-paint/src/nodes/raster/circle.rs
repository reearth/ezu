//! `circle` — centred disk source. Emits a canvas-sized `Raster`
//! (radius = `radius-frac × tile-size`) by default, or a `Sprite` at
//! `width-px × height-px` when `kind: "sprite"` (radius is then a
//! fraction of `min(width, height)`). Premultiplied RGBA output with
//! optional `hardness` edge falloff.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{read_color, read_number, read_number_or};
use crate::nodes::raster::generator_kind::{parse_generator_kind, GeneratorKind};

struct CircleNode {
    color: [f32; 4],
    radius_frac: f32,
    hardness: f32,
    out_kind: GeneratorKind,
}

impl Node for CircleNode {
    fn op_name(&self) -> &'static str {
        "circle"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        match self.out_kind {
            GeneratorKind::Raster => PortKind::Raster,
            GeneratorKind::Sprite { .. } => PortKind::Sprite,
        }
    }
    fn eval(&self, ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        // Raster mode anchors radius to `tile-size` (so the disk
        // visually scales with the tile geometry). Sprite mode
        // anchors to the shorter sprite side so the disk fits.
        let (out_w, out_h, radius_unit) = match self.out_kind {
            GeneratorKind::Raster => {
                let size = ctx.canvas.padded_size();
                (size, size, ctx.canvas.tile_size as f32)
            }
            GeneratorKind::Sprite { width, height } => (width, height, width.min(height) as f32),
        };
        let mut out = RasterBuf::new(out_w, out_h);
        let cx = out_w as f32 * 0.5;
        let cy = out_h as f32 * 0.5;
        let r = radius_unit * self.radius_frac;
        let h = self.hardness.clamp(0.0, 0.999);
        let inner = r * h;
        let [cr, cg, cb, ca] = self.color;
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
        for c in self.color {
            h.update(&c.to_le_bytes());
        }
        h.update(&self.radius_frac.to_le_bytes());
        h.update(&self.hardness.to_le_bytes());
        let (tag, dims) = self.out_kind.hash_tag();
        h.update(&tag);
        if let Some((w, hh)) = dims {
            h.update(&w.to_le_bytes());
            h.update(&hh.to_le_bytes());
        }
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
        let color = read_color(fields, "color", ctx)?;
        let radius_frac = read_number(fields, "radius-frac", ctx)? as f32;
        let hardness = read_number_or(fields, "hardness", ctx, 1.0)? as f32;
        let out_kind = parse_generator_kind(fields, ctx)?;
        Ok(BuiltNode {
            node: Box::new(CircleNode {
                color,
                radius_frac,
                hardness,
                out_kind,
            }),
            connections: vec![],
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
