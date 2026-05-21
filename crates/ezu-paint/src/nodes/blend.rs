//! `blend` — `Raster base + Raster over -> Raster`. Premultiplied
//! source-over with optional opacity, integer-only fast path.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::common::read_number_or;

struct BlendNode {
    opacity: f32,
}

impl Node for BlendNode {
    fn op_name(&self) -> &'static str {
        "blend"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[
            PortSpec {
                name: "base",
                kind: PortKind::Raster,
                optional: false,
            },
            PortSpec {
                name: "over",
                kind: PortKind::Raster,
                optional: false,
            },
        ];
        SPECS
    }
    fn output(&self) -> PortKind {
        PortKind::Raster
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let base = inputs[0]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("base".into()))?;
        let over = inputs[1]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("over".into()))?;
        if base.width != over.width || base.height != over.height {
            return Err(EvalError::Other("blend: size mismatch".into()));
        }
        // Premultiplied source-over with optional opacity. For a properly
        // premultiplied `over` buffer, scaling its alpha by `op` requires
        // scaling its colors by `op` too — hence a single `op_q` factor
        // applied to all four channels. Output stays in `[0, 255]` by
        // the premul invariant, so no saturation is needed.
        let mut out = RasterBuf::new(base.width, base.height);
        let op_q = (self.opacity.clamp(0.0, 1.0) * 255.0).round() as u16;
        let bp = &base.pixels;
        let op_buf = &over.pixels;
        let dst = &mut out.pixels;
        for i in (0..bp.len()).step_by(4) {
            let o0 = mul_u8q(op_buf[i], op_q);
            let o1 = mul_u8q(op_buf[i + 1], op_q);
            let o2 = mul_u8q(op_buf[i + 2], op_q);
            let oa = mul_u8q(op_buf[i + 3], op_q);
            let inv = 255u16 - oa as u16;
            dst[i] = o0 + mul_u8q(bp[i], inv);
            dst[i + 1] = o1 + mul_u8q(bp[i + 1], inv);
            dst[i + 2] = o2 + mul_u8q(bp[i + 2], inv);
            dst[i + 3] = oa + mul_u8q(bp[i + 3], inv);
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"blend");
        h.update(&self.opacity.to_le_bytes());
    }
}

pub(super) struct BlendFactory;
impl NodeFactory for BlendFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let base = take_input_ref(fields, "base")?;
        let over = take_input_ref(fields, "over")?;
        let opacity = read_number_or(fields, "opacity", ctx, 1.0)? as f32;
        Ok(BuiltNode {
            node: Box::new(BlendNode { opacity }),
            connections: vec![
                Connection {
                    port: "base".into(),
                    src: base,
                },
                Connection {
                    port: "over".into(),
                    src: over,
                },
            ],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Source-over composite (premultiplied) of `over` on top of `base`.",
            "properties": {
                "base": schema_frag::node_ref(),
                "over": schema_frag::node_ref(),
                "opacity": schema_frag::unit_number(),
            },
            "required": ["base", "over"],
        })
    }
}

/// Multiply a u8 channel by a 0..=255 quantized factor with proper
/// rounding: `(c * q + 127) / 255`. Result fits in `u8` for any
/// premul-correct alpha-over.
#[inline(always)]
fn mul_u8q(c: u8, q: u16) -> u8 {
    ((c as u16 * q + 127) / 255) as u8
}
