//! `color-to-alpha` — `Raster -> Raster`. Make pixels close to a
//! target color transparent (chroma-key style). Distance is Chebyshev
//! (max per-channel) in non-premultiplied sRGB; pixels within
//! `threshold` become fully transparent, pixels beyond `softness`
//! away are unaffected, with a linear ramp in between.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{read_color, read_number_or};

struct ColorToAlphaNode {
    color: [f32; 4],
    threshold: f32,
    softness: f32,
}

impl Node for ColorToAlphaNode {
    fn op_name(&self) -> &'static str {
        "color-to-alpha"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "input",
            accepts: &[PortKind::Raster],
            optional: false,
        }];
        SPECS
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let src = inputs[0]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let [tr, tg, tb, _] = self.color;
        let lo = self.threshold.max(0.0);
        let hi = (lo + self.softness).max(lo + 1e-6);
        let mut out = RasterBuf::new(src.width, src.height);
        for i in (0..src.pixels.len()).step_by(4) {
            let a = src.pixels[i + 3] as f32 / 255.0;
            if a <= 0.0 {
                continue;
            }
            let r = (src.pixels[i] as f32 / 255.0) / a;
            let g = (src.pixels[i + 1] as f32 / 255.0) / a;
            let b = (src.pixels[i + 2] as f32 / 255.0) / a;
            let d = (r - tr).abs().max((g - tg).abs()).max((b - tb).abs());
            let coverage = if d <= lo {
                0.0
            } else if d >= hi {
                1.0
            } else {
                (d - lo) / (hi - lo)
            };
            let new_a = a * coverage;
            out.pixels[i] = (r * new_a * 255.0).round() as u8;
            out.pixels[i + 1] = (g * new_a * 255.0).round() as u8;
            out.pixels[i + 2] = (b * new_a * 255.0).round() as u8;
            out.pixels[i + 3] = (new_a * 255.0).round() as u8;
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"color-to-alpha");
        for c in self.color {
            h.update(&c.to_le_bytes());
        }
        h.update(&self.threshold.to_le_bytes());
        h.update(&self.softness.to_le_bytes());
    }
}

pub(super) struct ColorToAlphaFactory;
impl NodeFactory for ColorToAlphaFactory {
    fn op_name(&self) -> &'static str {
        "color-to-alpha"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let color = read_color(fields, "color", ctx)?;
        let threshold = read_number_or(fields, "threshold", ctx, 0.0)? as f32;
        let softness = read_number_or(fields, "softness", ctx, 0.1)? as f32;
        Ok(BuiltNode {
            node: Box::new(ColorToAlphaNode {
                color,
                threshold,
                softness,
            }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Make pixels close to `color` transparent. Distance ≤ `threshold` → alpha 0, distance ≥ `threshold + softness` → alpha unchanged, linear in between. Distance is Chebyshev (max per-channel) in non-premultiplied sRGB.",
            "properties": {
                "input": schema_frag::node_ref(),
                "color": schema_frag::color(),
                "threshold": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.0 },
                "softness": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.1 },
            },
            "required": ["input", "color"],
        })
    }
}

ezu_graph::submit_node!(ColorToAlphaFactory);
