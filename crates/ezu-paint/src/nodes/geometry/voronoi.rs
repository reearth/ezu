//! `voronoi` — `Features -> Features`. Compute the Voronoi diagram of
//! the input's point set and emit the diagram's edges as polylines.
//! Polygons and lines in the input are ignored — pipe a `centroid`
//! upstream to derive seed points from them. Useful for stylised
//! "shattered glass" / "fence line" effects.
//!
//! Because it diagrams the pooled point set across every input feature, the
//! output is a single group with no per-feature properties.

use ezu_features::ops::voronoi::voronoi_edges;
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, FeatureGroup};

struct VoronoiNode;

impl Node for VoronoiNode {
    fn op_name(&self) -> &'static str {
        "voronoi"
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
        let points: Vec<(i32, i32)> = feats.points().collect();
        let lines = voronoi_edges(&points, None);
        if lines.is_empty() {
            return Ok(features_value(feats.extent, vec![]));
        }
        Ok(features_value(
            feats.extent,
            vec![FeatureGroup::synthetic(vec![], lines, vec![])],
        ))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"voronoi");
    }
}

pub(super) struct VoronoiFactory;
impl NodeFactory for VoronoiFactory {
    fn op_name(&self) -> &'static str {
        "voronoi"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        Ok(BuiltNode {
            node: Box::new(VoronoiNode),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Voronoi diagram of the input's point set. Output is a Features value carrying the Voronoi edges as polylines (each polyline is a single 2-point edge). Polygons and lines in the input are ignored — use `centroid` to derive points from them.",
            "properties": { "features": schema_frag::node_ref() },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(VoronoiFactory);
