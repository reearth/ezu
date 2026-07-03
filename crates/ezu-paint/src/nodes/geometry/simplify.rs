//! `simplify` — `Features -> Features`. Douglas-Peucker simplification
//! on every polyline and polygon ring. Points pass through unchanged.

use ezu_features::ops::simplify::{simplify_line, simplify_polygon};
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, FeatureGroup};

struct SimplifyNode {
    epsilon: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for SimplifyNode {
    fn op_name(&self) -> &'static str {
        "simplify"
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
        let epsilon = self.epsilon.get(ctx, inputs)?;
        // Per group: simplify each feature's lines and polygon rings (points
        // pass through), carrying properties.
        let mut out_groups = Vec::with_capacity(feats.groups.len());
        for g in &feats.groups {
            let lines: Vec<_> = g
                .lines
                .iter()
                .filter_map(|l| simplify_line(l, epsilon))
                .collect();
            let polygons: Vec<_> = g
                .polygons
                .iter()
                .filter_map(|p| simplify_polygon(p, epsilon))
                .collect();
            out_groups.push(FeatureGroup {
                properties: g.properties.clone(),
                polygons,
                lines,
                points: g.points.clone(),
            });
        }
        Ok(features_value(feats.extent, out_groups))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"simplify");
        self.epsilon.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        let mut r = InReader::new(fields, ctx, 1);
        let epsilon = r.number("epsilon")?;
        let parts = r.finish();

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
            node: Box::new(SimplifyNode {
                epsilon,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Douglas-Peucker simplify polylines and polygon rings; points pass through.",
            "properties": {
                "features": schema_frag::node_ref(),
                "epsilon": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                              "description": "Max perpendicular distance a vertex may be from the simplified line, in tile pixels." })),
            },
            "required": ["features", "epsilon"],
        })
    }
}

ezu_graph::submit_node!(SimplifyFactory);
