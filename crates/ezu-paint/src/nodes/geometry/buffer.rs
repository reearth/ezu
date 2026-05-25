//! `buffer` — `Features -> Features`. Minkowski sum / offset with a
//! disk of radius `distance` for every input geometry. Polygons accept
//! negative distances (erosion). Output is always polygons (points and
//! polylines become buffered areas).

use std::f64::consts::PI;

use ezu_features::ops::buffer::{
    buffer_lines, buffer_points, buffer_polygons, BufferJoin, BufferOpts,
};
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, read_number, read_optional_string};

struct BufferNode {
    distance: f64,
    join: BufferJoin,
}

impl Node for BufferNode {
    fn op_name(&self) -> &'static str {
        "buffer"
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
        let opts = BufferOpts {
            distance: self.distance,
            join: self.join,
        };
        let mut polygons = buffer_polygons(&feats.polygons, &opts);
        polygons.extend(buffer_lines(&feats.lines, &opts));
        polygons.extend(buffer_points(&feats.points, &opts));
        Ok(features_value(feats.extent, polygons, vec![], vec![]))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"buffer");
        h.update(&self.distance.to_le_bytes());
        match self.join {
            BufferJoin::Bevel => h.update(&[0]),
            BufferJoin::Miter { min_angle_rad } => {
                h.update(&[1]);
                h.update(&min_angle_rad.to_le_bytes());
            }
            BufferJoin::Round {
                max_segment_angle_rad,
            } => {
                h.update(&[2]);
                h.update(&max_segment_angle_rad.to_le_bytes());
            }
        }
    }
}

pub(super) struct BufferFactory;
impl NodeFactory for BufferFactory {
    fn op_name(&self) -> &'static str {
        "buffer"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let distance = read_number(fields, "distance", ctx)?;
        let join_kind =
            read_optional_string(fields, "join")?.unwrap_or_else(|| "miter".to_string());
        let join = match join_kind.as_str() {
            "bevel" => BufferJoin::Bevel,
            "round" => BufferJoin::Round {
                max_segment_angle_rad: 0.25,
            },
            "miter" => BufferJoin::Miter {
                min_angle_rad: 5.0 * PI / 180.0,
            },
            other => {
                return Err(FactoryError::BadField {
                    field: "join".into(),
                    msg: format!("unknown join '{other}', expected miter/round/bevel"),
                });
            }
        };
        Ok(BuiltNode {
            node: Box::new(BufferNode { distance, join }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Minkowski sum / offset with a disk. Polygons accept negative distance for erosion; lines and points are inflated by |distance|.",
            "properties": {
                "features": schema_frag::node_ref(),
                "distance": { "type": "number",
                              "description": "Offset distance in tile pixels. Positive inflates, negative erodes (polygons only)." },
                "join": { "type": "string", "enum": ["miter", "round", "bevel"], "default": "miter" },
            },
            "required": ["features", "distance"],
        })
    }
}

ezu_graph::submit_node!(BufferFactory);
