//! `tile-bounds` — `() -> Features`. Emits the tile's full
//! `[0, extent] × [0, extent]` rectangle as a single polygon. Useful
//! as a base for `fill-solid` (full-tile background), `hatch` (full-
//! tile pattern), or as a mask source.

use ezu_features::Polygon;
use ezu_graph::{
    BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::common::features_value;

const DEFAULT_EXTENT: u32 = 4096;

struct TileBoundsNode {
    extent: u32,
}

impl Node for TileBoundsNode {
    fn op_name(&self) -> &'static str {
        "tile-bounds"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self) -> PortKind {
        PortKind::Features
    }
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::Tile
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        _inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let e = self.extent as i32;
        let poly = Polygon {
            exterior: vec![(0, 0), (e, 0), (e, e), (0, e), (0, 0)],
            holes: vec![],
        };
        Ok(features_value(self.extent, vec![poly], vec![], vec![]))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"tile-bounds");
        h.update(&self.extent.to_le_bytes());
    }
}

pub(super) struct TileBoundsFactory;
impl NodeFactory for TileBoundsFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let extent = fields
            .get("extent")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .unwrap_or(DEFAULT_EXTENT);
        Ok(BuiltNode {
            node: Box::new(TileBoundsNode { extent }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Tile-filling rectangle polygon source.",
            "properties": {
                "extent": { "type": "integer", "minimum": 1, "default": DEFAULT_EXTENT },
            },
        })
    }
}
