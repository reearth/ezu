//! `simplify` — `Features -> Features`. Douglas-Peucker simplification
//! on every polyline and polygon ring. Points pass through unchanged.

use ezu_features::ops::simplify::{simplify_line, simplify_polygon};
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, read_number};

struct SimplifyNode {
    epsilon: f64,
}

impl Node for SimplifyNode {
    fn op_name(&self) -> &'static str {
        "simplify"
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
        let lines: Vec<_> = feats
            .lines
            .iter()
            .filter_map(|l| simplify_line(l, self.epsilon))
            .collect();
        let polygons: Vec<_> = feats
            .polygons
            .iter()
            .filter_map(|p| simplify_polygon(p, self.epsilon))
            .collect();
        Ok(features_value(
            feats.extent,
            polygons,
            lines,
            feats.points.clone(),
        ))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"simplify");
        h.update(&self.epsilon.to_le_bytes());
    }
}

pub(super) struct SimplifyFactory;
impl NodeFactory for SimplifyFactory {
    fn op_name(&self) -> &'static str {
        "simplify"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let epsilon = read_number(fields, "epsilon", ctx)?;
        Ok(BuiltNode {
            node: Box::new(SimplifyNode { epsilon }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Douglas-Peucker simplify polylines and polygon rings; points pass through.",
            "properties": {
                "features": schema_frag::node_ref(),
                "epsilon": { "type": "number", "minimum": 0.0,
                              "description": "Max perpendicular distance a vertex may be from the simplified line, in tile pixels." },
            },
            "required": ["features", "epsilon"],
        })
    }
}

ezu_graph::submit_node!(SimplifyFactory);
