//! `voronoi-fracture` — `(Features, Features) -> Features`. Split
//! each polygon in `features` into Voronoi sub-cells using the points
//! in `seeds` as Voronoi sites. The cells are clipped to the original
//! polygon, so the output never escapes the input shape.
//!
//! Use case: stained-glass / cracked-glass effects, or breaking up a
//! large polygon for differentiated styling.

use ezu_features::ops::voronoi::voronoi_fracture;
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, FeatureGroup};

struct VoronoiFractureNode;

impl Node for VoronoiFractureNode {
    fn op_name(&self) -> &'static str {
        "voronoi-fracture"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[
            PortSpec {
                name: "features",
                accepts: &[PortKind::Features],
                optional: false,
            },
            PortSpec {
                name: "seeds",
                accepts: &[PortKind::Features],
                optional: false,
            },
        ];
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
        let polys = downcast_features(
            inputs[0]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("features".into()))?,
        )?;
        let seeds = downcast_features(
            inputs[1]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("seeds".into()))?,
        )?;
        // Seeds are pooled across all seed features (the Voronoi sites are
        // global); fracture is applied per `features` group so each source
        // polygon's cells carry that feature's properties through.
        let seed_points: Vec<(i32, i32)> = seeds.points().collect();
        let mut out_groups = Vec::with_capacity(polys.groups.len());
        for g in &polys.groups {
            let mut out_polys = Vec::new();
            for polygon in &g.polygons {
                out_polys.extend(voronoi_fracture(polygon, &seed_points));
            }
            out_groups.push(FeatureGroup {
                properties: g.properties.clone(),
                polygons: out_polys,
                lines: vec![],
                points: vec![],
            });
        }
        Ok(features_value(polys.extent, out_groups))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"voronoi-fracture");
    }
}

pub(super) struct VoronoiFractureFactory;
impl NodeFactory for VoronoiFractureFactory {
    fn op_name(&self) -> &'static str {
        "voronoi-fracture"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let seeds = take_input_ref(fields, "seeds")?;
        Ok(BuiltNode {
            node: Box::new(VoronoiFractureNode),
            connections: vec![
                Connection {
                    port: "features".into(),
                    src: features,
                },
                Connection {
                    port: "seeds".into(),
                    src: seeds,
                },
            ],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Fracture each polygon in `features` into Voronoi sub-cells using the points in `seeds` as Voronoi sites. Cells are clipped against the source polygon. Lines/points on `features` and lines/polygons on `seeds` are ignored.",
            "properties": {
                "features": schema_frag::node_ref(),
                "seeds": schema_frag::node_ref(),
            },
            "required": ["features", "seeds"],
        })
    }
}

ezu_graph::submit_node!(VoronoiFractureFactory);
