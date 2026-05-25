//! `invert` — negate RGB channels over `Raster|Sprite` (pass-through);
//! alpha preserved. Operates in non-premultiplied space.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    raster_or_sprite_output, unwrap_raster_or_sprite, wrap_raster_like, ACCEPTS_RASTER_OR_SPRITE,
};

struct InvertNode;

impl Node for InvertNode {
    fn op_name(&self) -> &'static str {
        "invert"
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
        let mut out = RasterBuf::new(src.width, src.height);
        for i in (0..src.pixels.len()).step_by(4) {
            let a = src.pixels[i + 3];
            if a == 0 {
                continue;
            }
            // Invert in non-premultiplied space, then re-premultiply.
            // For premul input p = c * a, non-premul is c = p / a, so
            // inverted premul is (1 - p/a) * a = a - p.
            out.pixels[i] = a.saturating_sub(src.pixels[i]);
            out.pixels[i + 1] = a.saturating_sub(src.pixels[i + 1]);
            out.pixels[i + 2] = a.saturating_sub(src.pixels[i + 2]);
            out.pixels[i + 3] = a;
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"invert");
    }
}

pub(super) struct InvertFactory;
impl NodeFactory for InvertFactory {
    fn op_name(&self) -> &'static str {
        "invert"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        Ok(BuiltNode {
            node: Box::new(InvertNode),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Negate RGB channels (1 - c). Alpha is preserved.",
            "properties": { "input": schema_frag::node_ref() },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(InvertFactory);
