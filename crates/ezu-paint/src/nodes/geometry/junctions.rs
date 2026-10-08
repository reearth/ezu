//! `junctions` — `Features -> Features`. One point wherever input
//! polylines end or meet, using [`ezu_features::ops::junctions`], so a
//! line network split into sections can be marked with a cross tick at
//! every section boundary.
//!
//! Each junction is its own feature group with two properties and none
//! of the source features': `axis-deg`, the tick's axis in degrees
//! clockwise in `[0, 180)`, ready for `stamp`'s `rotation-deg-expr` with
//! a horizontal bar sprite; and `arms`, how many line ends meet there.
//! A junction merges ends from several features, so no one feature's
//! properties would describe it.
//!
//! Endpoints within `snap-px` tile pixels of each other merge. Loose ends
//! that a tile encoder made by cutting a line at the tile buffer are
//! dropped, so no tick appears along the clip line.

use std::collections::BTreeMap;
use std::sync::Arc;

use ezu_features::ops::junctions::junctions;
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, InfluenceCtx, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, FeatureGroup};

/// The property each junction's tick axis is stored under.
const AXIS_PROPERTY: &str = "axis-deg";
/// The property each junction's arm count is stored under.
const ARMS_PROPERTY: &str = "arms";

struct JunctionsNode {
    snap_px: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for JunctionsNode {
    fn op_name(&self) -> &'static str {
        "junctions"
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
    fn influence_pad(&self, ctx: &InfluenceCtx<'_>) -> u32 {
        // A junction sits at its lines' ends, moved by at most the snap
        // distance when ends merge.
        ctx.plus_bound(self.snap_px.static_bound())
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
        let scale = feats.extent as f64 / ctx.canvas.tile_w.max(1) as f64;
        let snap = self.snap_px.get(ctx, inputs)? * scale;
        let found = junctions(
            feats
                .groups
                .iter()
                .flat_map(|g| g.lines.iter().map(Vec::as_slice)),
            snap,
            feats.extent,
        );
        let out_groups = found
            .into_iter()
            .map(|j| {
                let properties: BTreeMap<String, maplibre_expr::Value> = [
                    (
                        AXIS_PROPERTY.to_string(),
                        maplibre_expr::Value::Number(j.axis_deg),
                    ),
                    (
                        ARMS_PROPERTY.to_string(),
                        maplibre_expr::Value::Number(j.arms as f64),
                    ),
                ]
                .into();
                FeatureGroup {
                    properties: Arc::new(properties),
                    polygons: vec![],
                    lines: vec![],
                    points: vec![j.point],
                }
            })
            .collect();
        Ok(features_value(feats.extent, out_groups))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"junctions");
        self.snap_px.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct JunctionsFactory;
impl NodeFactory for JunctionsFactory {
    fn op_name(&self) -> &'static str {
        "junctions"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let mut r = InReader::new(fields, ctx, 1);
        let snap_px = r.number_or("snap-px", 1.0)?;
        let parts = r.finish();
        if let Some(b) = snap_px.static_bound() {
            if b < 0.0 {
                return Err(FactoryError::BadField {
                    field: "snap-px".into(),
                    msg: "snap-px must be >= 0".into(),
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
            node: Box::new(JunctionsNode {
                snap_px,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "One point wherever polylines end or meet, for marking a line network split into sections (a block perimeter cut into frontages, a route cut into stages) with a cross tick at each section boundary. Both ends of every polyline count; polygons and points are ignored. Each junction is its own feature carrying `axis-deg` and `arms` and none of the source properties, since it may join several features. `axis-deg` is the tick's axis in degrees clockwise from the x axis, in [0, 180), so `stamp` with a horizontal bar sprite and `\"rotation-deg-expr\": [\"get\", \"axis-deg\"]` draws the tick: across the line at a loose end or where the line runs straight on, along the bisector at a corner, and through the middle of the widest gap between arms where the bisector would lie on or near an arm (a T-junction) or the corner is sharper than 40°. `arms` counts the line ends meeting there (1 for a loose end), for filtering. Loose ends a vector tile makes by cutting a line at its buffer are dropped: an end outside the tile, as far out as any geometry on that side, whose line runs back inward. A junction near a tile border comes out the same, with the same axis, from the tiles on either side. Feed it unstitched features: `stitch` cuts lines at the tile border, and the cut ends would meet as junctions there.",
            "properties": {
                "features": schema_frag::node_ref(),
                "snap-px": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "default": 1.0,
                    "description": "Line ends closer than this, in tile pixels, merge into one junction (chained: ends each within reach of a third merge too). Adjacent sections' shared ends do not always coincide exactly: separate digitising, or rounding before the data reached the tile, can leave them a fraction of a pixel apart. 1 px merges those without joining ends a reader would see apart. Raise it for data digitised less carefully." })),
            },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(JunctionsFactory);
