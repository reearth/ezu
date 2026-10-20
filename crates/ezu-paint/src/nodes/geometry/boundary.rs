//! `boundary` — `Features -> Features`. Replaces every input polygon
//! with its boundary rings (exterior + holes) as polylines. Existing
//! polylines and points pass through unchanged so the node can be
//! chained with `line`-style paint nodes to stroke polygon outlines.
//!
//! `clip-edges: "drop"` leaves out the edges a tile encoder added when it
//! clipped the polygon at the tile buffer, so a wide stroke on the outline
//! doesn't bleed into the tile from them and show a band along every tile
//! seam. It drops any segment outside the tile that runs within 15° of the
//! tile side it lies past — the tolerance keeps the option working after a
//! `buffer` closing has bent the clip line.

use ezu_features::ops::boundary::{polygon_boundary, polygon_boundary_without_clip_edges};
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EnumTable, EvalCtx, EvalError,
    FactoryCtx, FactoryError, InEnum, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, FeatureGroup};

/// `clip-edges`: whether to drop the edges a tile's clip added.
const CLIP_EDGES: EnumTable<bool> = &[("keep", false), ("drop", true)];

struct BoundaryNode {
    drop_clip_edges: InEnum<bool>,
    param_refs: Vec<String>,
}

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
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let feats = downcast_features(
            inputs[0]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("features".into()))?,
        )?;
        let drop_clip_edges = self.drop_clip_edges.get(ctx)?;
        // Per group: convert each feature's polygons to boundary polylines
        // (existing lines and points pass through), carrying properties.
        let mut out_groups = Vec::with_capacity(feats.groups.len());
        for g in &feats.groups {
            let mut lines = g.lines.clone();
            for p in &g.polygons {
                if drop_clip_edges {
                    lines.extend(polygon_boundary_without_clip_edges(p, feats.extent));
                } else {
                    lines.extend(polygon_boundary(p));
                }
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
        self.drop_clip_edges.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let mut r = InReader::new(fields, ctx, 1);
        let drop_clip_edges = r.enum_or("clip-edges", false, CLIP_EDGES)?;
        Ok(BuiltNode {
            node: Box::new(BoundaryNode {
                drop_clip_edges,
                param_refs: r.finish().param_refs,
            }),
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
                "clip-edges": schema_frag::in_enum(serde_json::json!({ "type": "string", "enum": ["keep", "drop"], "default": "keep",
                    "description": "What to do with the straight edges a vector tile adds where it clips a polygon at the tile buffer, just outside the tile. `keep` outlines them like any other edge; `drop` leaves them out, splitting a ring into open polylines where they were. Use `drop` when a wide stroke on the outline (a coastal band, say) shows a band along every tile seam: the stroke on those edges reaches into the tile. `drop` removes every segment that lies entirely past one side of the tile and runs within 15° of that side; the tolerance is what lets it work after a `buffer`, which can bend the clip line slightly. A real shoreline outside the tile running nearly parallel to its edge is dropped too, but nothing inside the tile goes missing, and a rectangle exactly on the tile border is kept." })),
            },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(BoundaryFactory);
