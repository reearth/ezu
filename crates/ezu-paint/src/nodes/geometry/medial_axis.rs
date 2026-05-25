//! `medial-axis` — `Features -> Features`. Approximate the medial
//! axis (skeleton) of each input polygon as a set of polylines.
//! Lines and points in the input are ignored.
//!
//! Internally densifies each polygon's boundary to `densify-px`
//! spacing, computes the Voronoi diagram of the resulting points,
//! and keeps Voronoi edges whose endpoints sit far enough from the
//! boundary to be on the actual axis. Branches shorter than
//! `min-branch-px` (after stitching) are pruned. Useful for river /
//! lake centrelines, label spines, leaf veins.

use ezu_features::ops::voronoi::medial_axis;
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, read_number_or};

struct MedialAxisNode {
    densify_px: f64,
    min_branch_px: f64,
}

impl Node for MedialAxisNode {
    fn op_name(&self) -> &'static str {
        "medial-axis"
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
        let mut lines = Vec::new();
        for polygon in &feats.polygons {
            lines.extend(medial_axis(polygon, self.densify_px, self.min_branch_px));
        }
        Ok(features_value(feats.extent, vec![], lines, vec![]))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"medial-axis");
        h.update(&self.densify_px.to_le_bytes());
        h.update(&self.min_branch_px.to_le_bytes());
    }
}

pub(super) struct MedialAxisFactory;
impl NodeFactory for MedialAxisFactory {
    fn op_name(&self) -> &'static str {
        "medial-axis"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let densify_px = read_number_or(fields, "densify-px", ctx, 4.0)?;
        let min_branch_px = read_number_or(fields, "min-branch-px", ctx, 8.0)?;
        if densify_px <= 0.0 {
            return Err(FactoryError::BadField {
                field: "densify-px".into(),
                msg: "must be > 0".into(),
            });
        }
        Ok(BuiltNode {
            node: Box::new(MedialAxisNode {
                densify_px,
                min_branch_px,
            }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Approximate medial axis of each input polygon, emitted as polylines. Smaller `densify-px` → more accurate but slower; `min-branch-px` prunes short side branches after stitching.",
            "properties": {
                "features": schema_frag::node_ref(),
                "densify-px": { "type": "number", "minimum": 0.5, "default": 4.0 },
                "min-branch-px": { "type": "number", "minimum": 0.0, "default": 8.0 },
            },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(MedialAxisFactory);
