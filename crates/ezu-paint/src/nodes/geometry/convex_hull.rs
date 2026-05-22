//! `convex-hull` — `Features -> Features`. Compute the convex hull of
//! every input vertex (points + lines + polygons pooled) and emit a
//! single polygon. Empty when there are fewer than 3 distinct
//! vertices.

use ezu_features::ops::convex_hull::convex_hull;
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value};

struct ConvexHullNode;

impl Node for ConvexHullNode {
    fn op_name(&self) -> &'static str {
        "convex-hull"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "features",
            kind: PortKind::Features,
            optional: false,
        }];
        SPECS
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
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let feats = downcast_features(
            inputs[0]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("features".into()))?,
        )?;
        let polygons = convex_hull(&feats.points, &feats.lines, &feats.polygons)
            .map(|p| vec![p])
            .unwrap_or_default();
        Ok(features_value(feats.extent, polygons, vec![], vec![]))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"convex-hull");
    }
}

pub(super) struct ConvexHullFactory;
impl NodeFactory for ConvexHullFactory {
    fn op_name(&self) -> &'static str {
        "convex-hull"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        Ok(BuiltNode {
            node: Box::new(ConvexHullNode),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Convex hull of every input vertex as a single polygon.",
            "properties": { "features": schema_frag::node_ref() },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(ConvexHullFactory);
