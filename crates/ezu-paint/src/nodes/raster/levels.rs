//! `levels` — Photoshop-style levels adjustment, pass-through over
//! `Raster|Sprite`. Maps `[in-black, in-white]` through a `gamma`
//! curve onto `[out-black, out-white]`. Alpha is preserved; the
//! mapping happens in non-premultiplied sRGB. Generalises
//! `brightness-contrast` (which is the linear case with `gamma = 1`).

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

struct LevelsNode {
    in_black: In<f64>,
    in_white: In<f64>,
    gamma: In<f64>,
    out_black: In<f64>,
    out_white: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for LevelsNode {
    fn op_name(&self) -> &'static str {
        "levels"
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
        let in_black = self.in_black.get(ctx, inputs)? as f32;
        let in_white = self.in_white.get(ctx, inputs)? as f32;
        let gamma = self.gamma.get(ctx, inputs)? as f32;
        let out_black = self.out_black.get(ctx, inputs)? as f32;
        let out_white = self.out_white.get(ctx, inputs)? as f32;
        let in_span = (in_white - in_black).max(1e-6);
        let inv_in = 1.0 / in_span;
        let inv_gamma = if gamma.abs() < 1e-6 { 1.0 } else { 1.0 / gamma };
        let out_lo = out_black;
        let out_span = out_white - out_black;
        let mut out = RasterBuf::new(src.width, src.height);
        for i in (0..src.pixels.len()).step_by(4) {
            let a = src.pixels[i + 3] as f32 / 255.0;
            if a <= 0.0 {
                continue;
            }
            for c in 0..3 {
                let p = (src.pixels[i + c] as f32 / 255.0) / a;
                let t = ((p - in_black) * inv_in).clamp(0.0, 1.0);
                let y = (t.powf(inv_gamma) * out_span + out_lo).clamp(0.0, 1.0);
                out.pixels[i + c] = (y * a * 255.0).round() as u8;
            }
            out.pixels[i + 3] = src.pixels[i + 3];
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"levels");
        self.in_black.param_hash(h);
        self.in_white.param_hash(h);
        self.gamma.param_hash(h);
        self.out_black.param_hash(h);
        self.out_white.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        let mut r = InReader::new(fields, ctx, 1);
        let in_black = r.number_or("in-black", 0.0)?;
        let in_white = r.number_or("in-white", 1.0)?;
        let gamma = r.number_or("gamma", 1.0)?;
        let out_black = r.number_or("out-black", 0.0)?;
        let out_white = r.number_or("out-white", 1.0)?;
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
            node: Box::new(LevelsNode {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Photoshop-style levels: remap input range [in-black, in-white] through `gamma` onto output range [out-black, out-white]. All values in [0, 1] (non-premultiplied sRGB). `gamma > 1` lightens midtones; `gamma < 1` darkens them.",
            "properties": {
                "input": schema_frag::node_ref(),
                "in-black": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.0 })),
                "in-white": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 1.0 })),
                "gamma": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.01, "default": 1.0 })),
                "out-black": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.0 })),
                "out-white": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 1.0 })),
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(LevelsFactory);
