//! `gradient-linear` — `() -> Raster`. Linear gradient between two
//! points. Coordinates are fractions: `[0, 0]` = top-left, `[1, 1]` =
//! bottom-right of the tile (tile-anchored) or of the full Mercator
//! world at z=0 (world-anchored).

use std::sync::Arc;

use ezu_graph::{
    BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{read_anchor, read_stops, read_xy, sample_stops, Anchor};
use crate::nodes::raster::gradient_common::pixel_to_user;

struct GradientLinearNode {
    start: [f32; 2],
    end: [f32; 2],
    stops: Vec<(f32, [f32; 4])>,
    anchor: Anchor,
}

impl Node for GradientLinearNode {
    fn op_name(&self) -> &'static str {
        "gradient-linear"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
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
        let dx = self.end[0] - self.start[0];
        let dy = self.end[1] - self.start[1];
        let len2 = dx * dx + dy * dy;
        if len2 < 1e-12 {
            // Degenerate (start == end): fill with first stop.
            let c = self.stops.first().map(|s| s.1).unwrap_or([0.0; 4]);
            let rgba = premul_u8(c);
            return Ok(PortValue::Raster(Arc::new(RasterBuf::filled(
                size, size, rgba,
            ))));
        }
        for y in 0..size {
            for x in 0..size {
                let (ux, uy) = pixel_to_user(x, y, ctx, self.anchor);
                let t = ((ux - self.start[0]) * dx + (uy - self.start[1]) * dy) / len2;
                let c = sample_stops(&self.stops, t);
                let i = ((y * size + x) * 4) as usize;
                let rgba = premul_u8(c);
                out.pixels[i] = rgba[0];
                out.pixels[i + 1] = rgba[1];
                out.pixels[i + 2] = rgba[2];
                out.pixels[i + 3] = rgba[3];
            }
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"gradient-linear");
        h.update(&self.start[0].to_le_bytes());
        h.update(&self.start[1].to_le_bytes());
        h.update(&self.end[0].to_le_bytes());
        h.update(&self.end[1].to_le_bytes());
        h.update(&[self.anchor as u8]);
        for (t, c) in &self.stops {
            h.update(&t.to_le_bytes());
            for v in c {
                h.update(&v.to_le_bytes());
            }
        }
    }
}

#[inline]
fn premul_u8(c: [f32; 4]) -> [u8; 4] {
    let a = c[3].clamp(0.0, 1.0);
    [
        (c[0].clamp(0.0, 1.0) * a * 255.0).round() as u8,
        (c[1].clamp(0.0, 1.0) * a * 255.0).round() as u8,
        (c[2].clamp(0.0, 1.0) * a * 255.0).round() as u8,
        (a * 255.0).round() as u8,
    ]
}

pub(super) struct GradientLinearFactory;
impl NodeFactory for GradientLinearFactory {
    fn op_name(&self) -> &'static str {
        "gradient-linear"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let start = read_xy(fields, "start", ctx, [0.0, 0.0])?;
        let end = read_xy(fields, "end", ctx, [1.0, 0.0])?;
        let stops = read_stops(fields, "stops", ctx)?;
        let anchor = read_anchor(fields, "anchor", ctx)?;
        Ok(BuiltNode {
            node: Box::new(GradientLinearNode {
                start,
                end,
                stops,
                anchor,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Linear gradient from `start` to `end` (fractional coords). Pixels project onto the start→end line; gradient parameter is the projected fraction.",
            "properties": {
                "start": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "default": [0.0, 0.0] },
                "end":   { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "default": [1.0, 0.0] },
                "stops": { "type": "array", "items": { "type": "array", "minItems": 2, "maxItems": 2 }, "minItems": 2 },
                "anchor": { "type": "string", "enum": ["tile", "world"], "default": "tile" },
            },
            "required": ["stops"],
        })
    }
}

ezu_graph::submit_node!(GradientLinearFactory);
