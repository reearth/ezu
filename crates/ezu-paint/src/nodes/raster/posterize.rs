//! `posterize` — `Raster|Sprite` pass-through. Quantise each colour
//! channel into `steps` evenly-spaced levels in non-premultiplied
//! sRGB. Alpha is preserved. Classic "screen print" / "painted-by-
//! numbers" look; pair with `levels` for finer control over which
//! tonal range gets banded.

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

struct PosterizeNode {
    steps: u32,
}

impl Node for PosterizeNode {
    fn op_name(&self) -> &'static str {
        "posterize"
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
        // `steps = 1` is degenerate (everything maps to 0). Clamp to
        // ≥ 2 so the output is always banded into at least two levels.
        let steps = self.steps.max(2);
        let levels = (steps - 1) as f32;
        let mut out = RasterBuf::new(src.width, src.height);
        for i in (0..src.pixels.len()).step_by(4) {
            let a = src.pixels[i + 3] as f32 / 255.0;
            if a <= 0.0 {
                continue;
            }
            for c in 0..3 {
                // Non-premultiplied component → quantise → re-premultiply.
                let p = (src.pixels[i + c] as f32 / 255.0) / a;
                let q = (p * levels).round() / levels;
                out.pixels[i + c] = (q.clamp(0.0, 1.0) * a * 255.0).round() as u8;
            }
            out.pixels[i + 3] = src.pixels[i + 3];
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"posterize");
        h.update(&self.steps.to_le_bytes());
    }
}

pub(super) struct PosterizeFactory;
impl NodeFactory for PosterizeFactory {
    fn op_name(&self) -> &'static str {
        "posterize"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let steps = read_number_or(fields, "steps", ctx, 4.0)?.round();
        if !(2.0..=256.0).contains(&steps) {
            return Err(FactoryError::BadField {
                field: "steps".into(),
                msg: "expected an integer in [2, 256]".into(),
            });
        }
        Ok(BuiltNode {
            node: Box::new(PosterizeNode {
                steps: steps as u32,
            }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Quantise each RGB channel into `steps` evenly-spaced levels (in non-premultiplied sRGB). Alpha preserved. Pair with `levels` to control which tonal band gets the strongest banding.",
            "properties": {
                "input": schema_frag::node_ref(),
                "steps": { "type": "integer", "minimum": 2, "maximum": 256, "default": 4 },
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(PosterizeFactory);
