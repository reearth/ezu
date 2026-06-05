//! `edge-detect` — Sobel gradient magnitude over `Raster|Sprite`
//! (pass-through). For each channel, computes
//! `sqrt(Gx² + Gy²) * strength` and clamps into `[0, 255]`. Grows
//! upstream pad by 1 so the 3×3 kernel stays in-bounds.
//!
//! Useful for stylised line-art outlines over a hillshade, or for
//! deriving an outline mask from a flat raster.

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

struct EdgeDetectNode {
    strength: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for EdgeDetectNode {
    fn op_name(&self) -> &'static str {
        "edge-detect"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        raster_or_sprite_output(input_kinds)
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + 1
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
        let strength = self.strength.get(ctx, inputs)?.max(0.0) as f32;
        let w = src.width;
        let h = src.height;
        let mut out = RasterBuf::new(w, h);
        // Sample with clamp-to-edge so we don't need an explicit
        // border. Edge pixels degenerate to a zero gradient on the
        // missing side, which is the expected behaviour for raster
        // ops that grow the upstream pad.
        let sample = |x: i32, y: i32, c: usize| -> i32 {
            let xc = x.clamp(0, w as i32 - 1) as u32;
            let yc = y.clamp(0, h as i32 - 1) as u32;
            src.pixels[((yc * w + xc) * 4) as usize + c] as i32
        };
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let off = ((y as u32 * w + x as u32) * 4) as usize;
                for c in 0..4 {
                    let p00 = sample(x - 1, y - 1, c);
                    let p10 = sample(x, y - 1, c);
                    let p20 = sample(x + 1, y - 1, c);
                    let p01 = sample(x - 1, y, c);
                    let p21 = sample(x + 1, y, c);
                    let p02 = sample(x - 1, y + 1, c);
                    let p12 = sample(x, y + 1, c);
                    let p22 = sample(x + 1, y + 1, c);
                    let gx = (p20 + 2 * p21 + p22) - (p00 + 2 * p01 + p02);
                    let gy = (p02 + 2 * p12 + p22) - (p00 + 2 * p10 + p20);
                    let mag = ((gx * gx + gy * gy) as f32).sqrt() * strength;
                    out.pixels[off + c] = mag.clamp(0.0, 255.0) as u8;
                }
            }
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"edge-detect");
        self.strength.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct EdgeDetectFactory;
impl NodeFactory for EdgeDetectFactory {
    fn op_name(&self) -> &'static str {
        "edge-detect"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let mut r = InReader::new(fields, ctx, 1);
        let strength = r.number_or("strength", 1.0)?;
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
            node: Box::new(EdgeDetectNode {
                strength,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Sobel gradient magnitude per channel, scaled by `strength` and clamped to [0, 255]. Pass-through over Raster / Sprite. Use after a hillshade for stylised outlines, or on a flat fill to derive an outline mask.",
            "properties": {
                "input": schema_frag::node_ref(),
                "strength": schema_frag::in_number(serde_json::json!({
                    "type": "number", "minimum": 0.0, "default": 1.0,
                    "description": "Scale factor applied to the gradient magnitude before clamping."
                })),
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(EdgeDetectFactory);
