//! `triangulate` — `Features -> Features`. Delaunay triangulation of
//! the input's point set. Each triangle is emitted as a closed
//! polygon. Polygons and lines in the input are ignored — feed
//! through `centroid` or `point-grid` upstream to derive points.

use ezu_features::ops::triangulate::triangulate;
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value};

struct TriangulateNode;

impl Node for TriangulateNode {
    fn op_name(&self) -> &'static str {
        "triangulate"
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
        let polys = triangulate(&feats.points);
        Ok(features_value(feats.extent, polys, vec![], vec![]))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"triangulate");
    }
}

pub(super) struct TriangulateFactory;
impl NodeFactory for TriangulateFactory {
    fn op_name(&self) -> &'static str {
        "triangulate"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        Ok(BuiltNode {
            node: Box::new(TriangulateNode),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Delaunay triangulation of the input's point set. Each triangle becomes a closed polygon. Polygons and lines on input are ignored.",
            "properties": { "features": schema_frag::node_ref() },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(TriangulateFactory);
