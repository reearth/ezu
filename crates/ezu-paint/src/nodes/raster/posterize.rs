//! `posterize` — `Raster|Sprite` pass-through. Quantise each colour
//! channel into `steps` evenly-spaced levels in non-premultiplied
//! sRGB. Alpha is preserved. Classic "screen print" / "painted-by-
//! numbers" look; pair with `levels` for finer control over which
//! tonal range gets banded.

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

/// The range `steps` is held to: a literal outside it fails the build, and
/// a `$param` or port value is rounded and clamped into it per render.
const STEPS_RANGE: std::ops::RangeInclusive<f64> = 2.0..=256.0;

struct PosterizeNode {
    steps: In<f64>,
    /// `input`, then a scalar port if `steps` is wired to one.
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for PosterizeNode {
    fn op_name(&self) -> &'static str {
        "posterize"
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
        // `steps = 1` is degenerate (everything maps to 0). Clamp to
        // ≥ 2 so the output is always banded into at least two levels;
        // the final `max` also catches a NaN, which casts to 0.
        let steps = (self
            .steps
            .get(ctx, inputs)?
            .round()
            .clamp(*STEPS_RANGE.start(), *STEPS_RANGE.end()) as u32)
            .max(2);
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
        // A literal hashes as the whole number it always did, so existing
        // styles keep their cache keys.
        match &self.steps {
            In::Const(v) => h.update(&(v.round() as u32).to_le_bytes()),
            steps => steps.param_hash(h),
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        let mut r = InReader::new(fields, ctx, 1);
        let steps = r.number_or("steps", 4.0)?;
        if let In::Const(v) = steps {
            if !STEPS_RANGE.contains(&v.round()) {
                return Err(FactoryError::BadField {
                    field: "steps".into(),
                    msg: "expected an integer in [2, 256]".into(),
                });
            }
        }
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
            node: Box::new(PosterizeNode {
                steps,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Quantise each RGB channel into `steps` evenly-spaced levels (in non-premultiplied sRGB). Alpha preserved. Pair with `levels` to control which tonal band gets the strongest banding.",
            "properties": {
                "input": schema_frag::node_ref(),
                "steps": schema_frag::in_number(serde_json::json!({ "type": "integer", "minimum": 2, "maximum": 256, "default": 4,
                           "description": "Levels per channel. A `$param` or `@node` value is rounded and held to [2, 256]." })),
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(PosterizeFactory);
