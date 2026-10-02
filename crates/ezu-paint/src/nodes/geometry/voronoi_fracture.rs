//! `voronoi-fracture` — `(Features, Features) -> Features`. Split
//! each polygon in `features` into Voronoi sub-cells using the points
//! in `seeds` as Voronoi sites. The cells are clipped to the original
//! polygon, so the output never escapes the input shape.
//!
//! Every cell is its own feature group. It carries its source polygon's
//! properties plus `random`, a value in `[0, 1)` drawn from its seed's
//! world position — so a `fill-expr` can give each cell its own tone,
//! and the tile on either side of a border gives a shared cell the same
//! one.
//!
//! Use case: stained-glass / cracked-glass effects, cobblestones,
//! per-brick colour, or breaking up a large polygon for differentiated
//! styling.

use std::collections::BTreeMap;
use std::sync::Arc;

use ezu_core::seed::{cell_seed, next_unit};
use ezu_features::ops::voronoi::voronoi_fracture;
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, FeatureGroup};

/// Salt for a cell's `random`, so it does not correlate with other
/// world-seeded jitter.
const CELL_RANDOM_SALT: u32 = 0x7E5_5E11;

/// The property each cell's random value is stored under.
const RANDOM_PROPERTY: &str = "random";

struct VoronoiFractureNode {
    aspect: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for VoronoiFractureNode {
    fn op_name(&self) -> &'static str {
        "voronoi-fracture"
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
        let aspect = self.aspect.get(ctx, inputs)?;
        // Seeds are pooled across all seed features (the Voronoi sites are
        // global); fracture is applied per `features` group so each source
        // polygon's cells carry that feature's properties through.
        let seed_points: Vec<(i32, i32)> = seeds.points().collect();
        // A seed's world position, in the seeds' extent units at this zoom.
        // Integers, so neighbouring tiles derive exactly the same value.
        let e = seeds.extent as i64;
        let (ox, oy) = (ctx.tile.x as i64 * e, ctx.tile.y as i64 * e);
        let salt = CELL_RANDOM_SALT ^ u32::from(ctx.tile.z);
        let mut out_groups = Vec::new();
        for g in &polys.groups {
            for polygon in &g.polygons {
                for cell in voronoi_fracture(polygon, &seed_points, aspect) {
                    let (sx, sy) = cell.site;
                    let mut state = cell_seed(ox + sx as i64, oy + sy as i64, salt);
                    let random = f64::from(next_unit(&mut state));
                    let mut properties: BTreeMap<String, maplibre_expr::Value> =
                        (*g.properties).clone();
                    properties.insert(
                        RANDOM_PROPERTY.to_string(),
                        maplibre_expr::Value::Number(random),
                    );
                    out_groups.push(FeatureGroup {
                        properties: Arc::new(properties),
                        polygons: vec![cell.polygon],
                        lines: vec![],
                        points: vec![],
                    });
                }
            }
        }
        Ok(features_value(polys.extent, out_groups))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"voronoi-fracture");
        self.aspect.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let seeds = take_input_ref(fields, "seeds")?;
        let mut r = InReader::new(fields, ctx, 2);
        let aspect = r.number_or("aspect", 1.0)?;
        let parts = r.finish();
        if let Some(b) = aspect.static_bound() {
            if b <= 0.0 {
                return Err(FactoryError::BadField {
                    field: "aspect".into(),
                    msg: "aspect must be > 0".into(),
                });
            }
        }

        let mut ports = vec![
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
        ports.extend(parts.ports);
        let mut connections = vec![
            Connection {
                port: "features".into(),
                src: features,
            },
            Connection {
                port: "seeds".into(),
                src: seeds,
            },
        ];
        connections.extend(parts.connections);
        Ok(BuiltNode {
            node: Box::new(VoronoiFractureNode {
                aspect,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Fracture each polygon in `features` into Voronoi sub-cells using the points in `seeds` as Voronoi sites. Cells are clipped against the source polygon. Each cell is its own feature, carrying the source polygon's properties plus `random`: a value in [0, 1) drawn from its seed's world position, the same in every tile, for per-cell colour through a `fill-expr` such as `[\"interpolate\", [\"linear\"], [\"get\", \"random\"], 0, \"#8c3a24\", 1, \"#b65a38\"]`. Lines/points on `features` and lines/polygons on `seeds` are ignored.",
            "properties": {
                "features": schema_frag::node_ref(),
                "seeds": schema_frag::node_ref(),
                "aspect": schema_frag::in_number(serde_json::json!({
                    "type": "number", "exclusiveMinimum": 0.0, "default": 1.0,
                    "description": "Weight on vertical distance when choosing each point's nearest seed. `> 1` stretches cells along X, `< 1` along Y — seeds on a staggered `point-grid` with `aspect` well above 1 give the rectangles of a brick bond rather than hexagons."
                })),
            },
            "required": ["features", "seeds"],
        })
    }
}

ezu_graph::submit_node!(VoronoiFractureFactory);
