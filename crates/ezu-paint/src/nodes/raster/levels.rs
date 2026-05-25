//! `levels` — Photoshop-style levels adjustment, pass-through over
//! `Raster|Sprite`. Maps `[in-black, in-white]` through a `gamma`
//! curve onto `[out-black, out-white]`. Alpha is preserved; the
//! mapping happens in non-premultiplied sRGB. Generalises
//! `brightness-contrast` (which is the linear case with `gamma = 1`).

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    raster_or_sprite_output, read_number_or, unwrap_raster_or_sprite, wrap_raster_like,
    ACCEPTS_RASTER_OR_SPRITE,
};

struct LevelsNode {
    in_black: f32,
    in_white: f32,
    gamma: f32,
    out_black: f32,
    out_white: f32,
}

impl Node for LevelsNode {
    fn op_name(&self) -> &'static str {
        "levels"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "input",
            accepts: ACCEPTS_RASTER_OR_SPRITE,
            optional: false,
        }];
        SPECS
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        raster_or_sprite_output(input_kinds)
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let (src, kind) = unwrap_raster_or_sprite(input, "input")?;
        let in_span = (self.in_white - self.in_black).max(1e-6);
        let inv_in = 1.0 / in_span;
        let inv_gamma = if self.gamma.abs() < 1e-6 {
            1.0
        } else {
            1.0 / self.gamma
        };
        let out_lo = self.out_black;
        let out_span = self.out_white - self.out_black;
        let mut out = RasterBuf::new(src.width, src.height);
        for i in (0..src.pixels.len()).step_by(4) {
            let a = src.pixels[i + 3] as f32 / 255.0;
            if a <= 0.0 {
                continue;
            }
            for c in 0..3 {
                let p = (src.pixels[i + c] as f32 / 255.0) / a;
                let t = ((p - self.in_black) * inv_in).clamp(0.0, 1.0);
                let y = (t.powf(inv_gamma) * out_span + out_lo).clamp(0.0, 1.0);
                out.pixels[i + c] = (y * a * 255.0).round() as u8;
            }
            out.pixels[i + 3] = src.pixels[i + 3];
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"levels");
        h.update(&self.in_black.to_le_bytes());
        h.update(&self.in_white.to_le_bytes());
        h.update(&self.gamma.to_le_bytes());
        h.update(&self.out_black.to_le_bytes());
        h.update(&self.out_white.to_le_bytes());
    }
}

pub(super) struct LevelsFactory;
impl NodeFactory for LevelsFactory {
    fn op_name(&self) -> &'static str {
        "levels"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let in_black = read_number_or(fields, "in-black", ctx, 0.0)? as f32;
        let in_white = read_number_or(fields, "in-white", ctx, 1.0)? as f32;
        let gamma = read_number_or(fields, "gamma", ctx, 1.0)? as f32;
        let out_black = read_number_or(fields, "out-black", ctx, 0.0)? as f32;
        let out_white = read_number_or(fields, "out-white", ctx, 1.0)? as f32;
        Ok(BuiltNode {
            node: Box::new(LevelsNode {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
            }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Photoshop-style levels: remap input range [in-black, in-white] through `gamma` onto output range [out-black, out-white]. All values in [0, 1] (non-premultiplied sRGB). `gamma > 1` lightens midtones; `gamma < 1` darkens them.",
            "properties": {
                "input": schema_frag::node_ref(),
                "in-black": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.0 },
                "in-white": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 1.0 },
                "gamma": { "type": "number", "minimum": 0.01, "default": 1.0 },
                "out-black": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.0 },
                "out-white": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 1.0 },
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(LevelsFactory);
