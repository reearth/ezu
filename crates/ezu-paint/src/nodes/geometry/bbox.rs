//! `bbox` — `Features -> Features`. Pool every input vertex (across
//! points, lines, and polygons) and emit a single axis-aligned
//! bounding-box polygon. Useful as a clipping or placement reference
//! for downstream ops. Empty input yields empty output.

use ezu_features::ops::bbox::bbox_polygon;
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value};

struct BBoxNode;

impl Node for BBoxNode {
    fn op_name(&self) -> &'static str {
        "bbox"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "features",
            accepts: &[PortKind::Features],
            optional: false,
        }];
        SPECS
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Features
    }
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::Tile
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let feats = downcast_features(
            inputs[0]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("features".into()))?,
        )?;
        let polys = bbox_polygon(&feats.points, &feats.lines, &feats.polygons)
            .map(|p| vec![p])
            .unwrap_or_default();
        Ok(features_value(feats.extent, polys, vec![], vec![]))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"bbox");
    }
}

pub(super) struct BBoxFactory;
impl NodeFactory for BBoxFactory {
    fn op_name(&self) -> &'static str {
        "bbox"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        Ok(BuiltNode {
            node: Box::new(BBoxNode),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Axis-aligned bounding box of every input vertex (pooled across polygons, lines, and points), emitted as a single rectangular polygon.",
            "properties": { "features": schema_frag::node_ref() },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(BBoxFactory);
