//! `gradient-diamond` — `() -> Raster`. Diamond gradient (Manhattan
//! distance from `center`). Useful for retro / pixel-art halos and
//! square decorations.

use std::sync::Arc;

use ezu_graph::{
    BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    read_anchor, read_number_or, read_stops, read_xy, sample_stops, Anchor,
};
use crate::nodes::raster::gradient_common::pixel_to_user;

struct GradientDiamondNode {
    center: [f32; 2],
    radius: f32,
    stops: Vec<(f32, [f32; 4])>,
    anchor: Anchor,
}

impl Node for GradientDiamondNode {
    fn op_name(&self) -> &'static str {
        "gradient-diamond"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self) -> PortKind {
        PortKind::Raster
    }
    fn coord_space(&self) -> CoordSpace {
        match self.anchor {
            Anchor::Tile => CoordSpace::Tile,
            Anchor::World => CoordSpace::World,
        }
    }
    fn eval(&self, ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        let size = ctx.canvas.padded_size();
        let mut out = RasterBuf::new(size, size);
        let r = self.radius.max(1e-6);
        for y in 0..size {
            for x in 0..size {
                let (ux, uy) = pixel_to_user(x, y, ctx, self.anchor);
                let t = ((ux - self.center[0]).abs() + (uy - self.center[1]).abs()) / r;
                let c = sample_stops(&self.stops, t);
                let i = ((y * size + x) * 4) as usize;
                let a = c[3].clamp(0.0, 1.0);
                out.pixels[i] = (c[0].clamp(0.0, 1.0) * a * 255.0).round() as u8;
                out.pixels[i + 1] = (c[1].clamp(0.0, 1.0) * a * 255.0).round() as u8;
                out.pixels[i + 2] = (c[2].clamp(0.0, 1.0) * a * 255.0).round() as u8;
                out.pixels[i + 3] = (a * 255.0).round() as u8;
            }
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"gradient-diamond");
        h.update(&self.center[0].to_le_bytes());
        h.update(&self.center[1].to_le_bytes());
        h.update(&self.radius.to_le_bytes());
        h.update(&[self.anchor as u8]);
        for (t, c) in &self.stops {
            h.update(&t.to_le_bytes());
            for v in c {
                h.update(&v.to_le_bytes());
            }
        }
    }
}

pub(super) struct GradientDiamondFactory;
impl NodeFactory for GradientDiamondFactory {
    fn op_name(&self) -> &'static str {
        "gradient-diamond"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let center = read_xy(fields, "center", ctx, [0.5, 0.5])?;
        let radius = read_number_or(fields, "radius", ctx, 0.5)? as f32;
        let stops = read_stops(fields, "stops", ctx)?;
        let anchor = read_anchor(fields, "anchor", ctx)?;
        Ok(BuiltNode {
            node: Box::new(GradientDiamondNode {
                center,
                radius,
                stops,
                anchor,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Diamond gradient from `center`. Gradient parameter is Manhattan (|dx| + |dy|) distance / radius.",
            "properties": {
                "center": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "default": [0.5, 0.5] },
                "radius": { "type": "number", "minimum": 0.0, "default": 0.5 },
                "stops": { "type": "array", "items": { "type": "array", "minItems": 2, "maxItems": 2 }, "minItems": 2 },
                "anchor": { "type": "string", "enum": ["tile", "world"], "default": "tile" },
            },
            "required": ["stops"],
        })
    }
}

ezu_graph::submit_node!(GradientDiamondFactory);
