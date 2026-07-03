//! `boundary` — `Features -> Features`. Replaces every input polygon
//! with its boundary rings (exterior + holes) as polylines. Existing
//! polylines and points pass through unchanged so the node can be
//! chained with `line`-style paint nodes to stroke polygon outlines.

use ezu_features::ops::boundary::polygon_boundary;
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, FeatureGroup};

struct BoundaryNode;

impl Node for BoundaryNode {
    fn op_name(&self) -> &'static str {
        "boundary"
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
        // Per group: convert each feature's polygons to boundary polylines
        // (existing lines and points pass through), carrying properties.
        let mut out_groups = Vec::with_capacity(feats.groups.len());
        for g in &feats.groups {
            let mut lines = g.lines.clone();
            for p in &g.polygons {
                lines.extend(polygon_boundary(p));
            }
            out_groups.push(FeatureGroup {
                properties: g.properties.clone(),
                polygons: vec![],
                lines,
                points: g.points.clone(),
            });
        }
        Ok(features_value(feats.extent, out_groups))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"boundary");
    }
}

pub(super) struct BoundaryFactory;
impl NodeFactory for BoundaryFactory {
    fn op_name(&self) -> &'static str {
        "boundary"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        Ok(BuiltNode {
            node: Box::new(BoundaryNode),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Convert each polygon to its boundary rings (exterior + holes) as polylines. Existing polylines and points pass through.",
            "properties": {
                "features": schema_frag::node_ref(),
            },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(BoundaryFactory);
