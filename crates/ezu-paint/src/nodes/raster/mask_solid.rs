//! `mask-solid` — `() -> Mask`. Uniform-value mask source.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, MaskBuf, Node,
    NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::read_number;

struct MaskSolidNode {
    value: f32,
}

impl Node for MaskSolidNode {
    fn op_name(&self) -> &'static str {
        "mask-solid"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self) -> PortKind {
        PortKind::Mask
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        _: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let size = ctx.canvas.padded_size();
        Ok(PortValue::Mask(Arc::new(MaskBuf::filled(
            size, size, self.value,
        ))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"mask-solid");
        h.update(&self.value.to_le_bytes());
    }
}

pub(super) struct MaskSolidFactory;
impl NodeFactory for MaskSolidFactory {
    fn op_name(&self) -> &'static str { "mask-solid" }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let value = read_number(fields, "value", ctx)? as f32;
        Ok(BuiltNode {
            node: Box::new(MaskSolidNode { value }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Uniform-value mask source.",
            "properties": { "value": schema_frag::unit_number() },
            "required": ["value"],
        })
    }
}

ezu_graph::submit_node!(MaskSolidFactory);
