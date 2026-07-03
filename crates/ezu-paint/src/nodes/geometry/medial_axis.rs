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
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, FeatureGroup};

struct MedialAxisNode {
    densify_px: In<f64>,
    min_branch_px: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for MedialAxisNode {
    fn op_name(&self) -> &'static str {
        "medial-axis"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Features
    }
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::Tile
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let feats = downcast_features(
            inputs[0]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("features".into()))?,
        )?;
        let densify_px = self.densify_px.get(ctx, inputs)?;
        let min_branch_px = self.min_branch_px.get(ctx, inputs)?;
        // Per group: skeletonise each feature's polygons into polylines,
        // carrying properties.
        let mut out_groups = Vec::with_capacity(feats.groups.len());
        for g in &feats.groups {
            let mut lines = Vec::new();
            for polygon in &g.polygons {
                lines.extend(medial_axis(polygon, densify_px, min_branch_px));
            }
            out_groups.push(FeatureGroup {
                properties: g.properties.clone(),
                polygons: vec![],
                lines,
                points: vec![],
            });
        }
        Ok(features_value(feats.extent, out_groups))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"medial-axis");
        self.densify_px.param_hash(h);
        self.min_branch_px.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        let mut r = InReader::new(fields, ctx, 1);
        let densify_px = r.number_or("densify-px", 4.0)?;
        let min_branch_px = r.number_or("min-branch-px", 8.0)?;
        let parts = r.finish();

        // densify-px must be > 0; check the static bound (literal, or a
        // `$param`'s declared `max`). A `@node` port has no static bound —
        // the underlying op returns no axis for non-positive values.
        if let Some(b) = densify_px.static_bound() {
            if b <= 0.0 {
                return Err(FactoryError::BadField {
                    field: "densify-px".into(),
                    msg: "densify-px must be > 0".into(),
                });
            }
        }

        let mut ports = vec![PortSpec {
            name: "features",
            accepts: &[PortKind::Features],
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "features".into(),
            src: features,
        }];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(MedialAxisNode {
                densify_px,
                min_branch_px,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Approximate medial axis of each input polygon, emitted as polylines. Smaller `densify-px` → more accurate but slower; `min-branch-px` prunes short side branches after stitching.",
            "properties": {
                "features": schema_frag::node_ref(),
                "densify-px": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.5, "default": 4.0 })),
                "min-branch-px": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "default": 8.0 })),
            },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(MedialAxisFactory);
