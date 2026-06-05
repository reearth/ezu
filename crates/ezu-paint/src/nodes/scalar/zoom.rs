//! `zoom` — the current tile's zoom level as a `Scalar` number.
//! Feed it through `math` for zoom-dependent widths, opacities, etc.:
//!
//! ```json
//! "z":     { "op": "zoom" },
//! "width": { "op": "math", "fn": "mul", "a": "@z", "b": 0.5 },
//! "roads": { "op": "line", "features": "@feat", "width-px": "@width", ... }
//! ```

use ezu_graph::{
    BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory, PortKind, PortSpec,
    PortValue, ScalarValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

struct ZoomNode;

impl Node for ZoomNode {
    fn op_name(&self) -> &'static str {
        "zoom"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Scalar
    }
    fn eval(&self, ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        Ok(PortValue::Scalar(ScalarValue::Number(ctx.tile.z as f64)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"zoom");
    }
}

pub(super) struct ZoomFactory;
impl NodeFactory for ZoomFactory {
    fn op_name(&self) -> &'static str {
        "zoom"
    }
    fn build(
        &self,
        _fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        Ok(BuiltNode {
            node: Box::new(ZoomNode),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "The current tile's zoom level as a scalar number. Combine with `math` for zoom-dependent styling.",
            "properties": {},
        })
    }
}

ezu_graph::submit_node!(ZoomFactory);
