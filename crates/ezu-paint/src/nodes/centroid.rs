//! `centroid` — `Features -> Features`. Replaces the input geometry
//! with one point per polygon and/or polyline using
//! [`ezu_features::ops::centroid`]. Useful for label anchoring or
//! point-symbol placement derived from area / line features.

use ezu_features::ops::centroid::{linestring_centroid, polygon_centroid};
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError,
    FactoryCtx, FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::common::{downcast_features, features_value};

struct CentroidNode {
    include_polygons: bool,
    include_lines: bool,
}

impl Node for CentroidNode {
    fn op_name(&self) -> &'static str {
        "centroid"
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
        let mut points = Vec::new();
        if self.include_polygons {
            points.extend(feats.polygons.iter().filter_map(polygon_centroid));
        }
        if self.include_lines {
            points.extend(
                feats
                    .lines
                    .iter()
                    .filter_map(|l| linestring_centroid(l)),
            );
        }
        Ok(features_value(feats.extent, vec![], vec![], points))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"centroid");
        h.update(&[self.include_polygons as u8, self.include_lines as u8]);
    }
}

pub(super) struct CentroidFactory;
impl NodeFactory for CentroidFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let include_polygons = fields
            .get("include-polygons")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let include_lines = fields
            .get("include-lines")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        Ok(BuiltNode {
            node: Box::new(CentroidNode {
                include_polygons,
                include_lines,
            }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Replace input geometry with one point per polygon / polyline centroid.",
            "properties": {
                "features": schema_frag::node_ref(),
                "include-polygons": { "type": "boolean", "default": true },
                "include-lines": { "type": "boolean", "default": true },
            },
            "required": ["features"],
        })
    }
}
