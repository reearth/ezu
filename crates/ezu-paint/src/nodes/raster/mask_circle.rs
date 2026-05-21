//! `mask-circle` — `() -> Mask`. Centered disk, radius given as a
//! fraction of `tile-size`. Useful for testing without MVT input.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, MaskBuf, Node,
    NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{read_number, read_number_or};

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

pub(super) struct MaskCircleFactory;
impl NodeFactory for MaskCircleFactory {
    fn op_name(&self) -> &'static str { "mask-circle" }
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

ezu_graph::submit_node!(MaskCircleFactory);
