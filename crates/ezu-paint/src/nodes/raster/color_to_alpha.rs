//! `color-to-alpha` — chroma-key over `Raster|Sprite` (pass-through).
//! Make pixels close to a target color transparent. Distance is
//! Chebyshev (max per-channel) in non-premultiplied sRGB; pixels
//! within `threshold` become fully transparent, pixels beyond
//! `softness` away are unaffected, with a linear ramp in between.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    raster_or_sprite_output, unwrap_raster_or_sprite, wrap_raster_like, ACCEPTS_RASTER_OR_SPRITE,
};

struct ColorToAlphaNode {
    color: In<[f32; 4]>,
    threshold: In<f64>,
    softness: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for ColorToAlphaNode {
    fn op_name(&self) -> &'static str {
        "color-to-alpha"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        raster_or_sprite_output(input_kinds)
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let (src, kind) = unwrap_raster_or_sprite(input, "input")?;
        let [tr, tg, tb, _] = self.color.get(ctx, inputs)?;
        let threshold = self.threshold.get(ctx, inputs)? as f32;
        let softness = self.softness.get(ctx, inputs)? as f32;
        let lo = threshold.max(0.0);
        let hi = (lo + softness).max(lo + 1e-6);
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
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"color-to-alpha");
        self.color.param_hash(h);
        self.threshold.param_hash(h);
        self.softness.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        let mut r = InReader::new(fields, ctx, 1);
        let color = r.color("color")?;
        let threshold = r.number_or("threshold", 0.0)?;
        let softness = r.number_or("softness", 0.1)?;
        let parts = r.finish();

        let mut ports = vec![PortSpec {
            name: "input",
            accepts: ACCEPTS_RASTER_OR_SPRITE,
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "input".into(),
            src: input,
        }];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(ColorToAlphaNode {
                color,
                threshold,
                softness,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Make pixels close to `color` transparent. Distance ≤ `threshold` → alpha 0, distance ≥ `threshold + softness` → alpha unchanged, linear in between. Distance is Chebyshev (max per-channel) in non-premultiplied sRGB.",
            "properties": {
                "input": schema_frag::node_ref(),
                "color": schema_frag::color(),
                "threshold": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.0 })),
                "softness": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.1 })),
            },
            "required": ["input", "color"],
        })
    }
}

ezu_graph::submit_node!(ColorToAlphaFactory);
