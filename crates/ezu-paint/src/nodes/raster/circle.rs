//! `circle` — `() -> Raster`. Centered disk, radius given as a
//! fraction of `tile-size`. Premultiplied RGBA output with optional
//! `hardness` edge falloff.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{read_color, read_number, read_number_or};

struct CircleNode {
    color: [f32; 4],
    radius_frac: f32,
    hardness: f32,
}

impl Node for CircleNode {
    fn op_name(&self) -> &'static str {
        "circle"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self) -> PortKind {
        PortKind::Raster
    }
    fn eval(&self, ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        let size = ctx.canvas.padded_size();
        let mut out = RasterBuf::new(size, size);
        let cx = size as f32 * 0.5;
        let cy = size as f32 * 0.5;
        let r = ctx.canvas.tile_size as f32 * self.radius_frac;
        let h = self.hardness.clamp(0.0, 0.999);
        let inner = r * h;
        let [cr, cg, cb, ca] = self.color;
        for y in 0..size {
            for x in 0..size {
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
                let i = ((y * size + x) * 4) as usize;
                out.pixels[i] = (cr * alpha * 255.0).round() as u8;
                out.pixels[i + 1] = (cg * alpha * 255.0).round() as u8;
                out.pixels[i + 2] = (cb * alpha * 255.0).round() as u8;
                out.pixels[i + 3] = (alpha * 255.0).round() as u8;
            }
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"circle");
        for c in self.color {
            h.update(&c.to_le_bytes());
        }
        h.update(&self.radius_frac.to_le_bytes());
        h.update(&self.hardness.to_le_bytes());
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
        Ok(BuiltNode {
            node: Box::new(CircleNode {
                color,
                radius_frac,
                hardness,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Centered disk raster source. Radius is a fraction of `tile-size`.",
            "properties": {
                "color": schema_frag::color(),
                "radius-frac": schema_frag::unit_number(),
                "hardness": schema_frag::unit_number(),
            },
            "required": ["color", "radius-frac"],
        })
    }
}

ezu_graph::submit_node!(CircleFactory);
