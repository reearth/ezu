//! `solid` — `() -> Raster`. Constant-color fill of the entire canvas.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::common::{color_to_premul_u8, read_color};

struct SolidNode {
    color: [f32; 4],
}

impl Node for SolidNode {
    fn op_name(&self) -> &'static str {
        "solid"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self) -> PortKind {
        PortKind::Raster
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        _: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let size = ctx.canvas.padded_size();
        let rgba = color_to_premul_u8(self.color);
        Ok(PortValue::Raster(Arc::new(RasterBuf::filled(
            size, size, rgba,
        ))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"solid");
        for c in self.color {
            h.update(&c.to_le_bytes());
        }
    }
}

pub(super) struct SolidFactory;
impl NodeFactory for SolidFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let color = read_color(fields, "color", ctx)?;
        Ok(BuiltNode {
            node: Box::new(SolidNode { color }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Solid-color raster source filling the entire canvas.",
            "properties": { "color": schema_frag::color() },
            "required": ["color"],
        })
    }
}
