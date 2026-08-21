//! `dot-density` — `Features -> Features`. Scatter points inside input
//! polygons at a density read from each feature, the geometry behind a
//! dot density map: one dot stands for `dot-value` units of whatever is
//! being mapped, and the dots are spread over the feature rather than
//! summarized into a single colour.
//!
//! The output is points, so the dots themselves are drawn by whatever
//! comes next — `circles` for plain dots, `stamp` for a sprite. Feature
//! properties are carried through, so the dot layer can still be styled
//! per feature (one hue per category, for a multivariate dot map).
//!
//! ```json
//! "dots": {
//!   "op": "dot-density",
//!   "features": "@tracts",
//!   "density-expr": ["/", ["get", "POP"], ["get", "AREA_KM2"]],
//!   "dot-value": 100
//! },
//! "paint": { "op": "circles", "features": "@dots", "radius": 1.2 }
//! ```
//!
//! `density-expr` is in units per square kilometre, not units — the
//! scatter needs a density, and how a feature's total becomes a density
//! is the author's decision to make and to state. Dividing a count by an
//! area attribute is the usual answer.
//!
//! Dots are placed on a world-anchored lattice (see
//! [`ezu_features::ops::scatter`]), so they do not break at tile seams.
//! Two consequences worth knowing: a cell holds at most one dot, so a
//! target above `1 / spacing-px²` dots per screen pixel saturates — lower
//! `spacing-px` or raise `dot-value` — and because the lattice is sized
//! in screen pixels, the dots re-scatter from one zoom level to the next
//! while their density stays true.

use ezu_core::coord::metres_per_world_unit;
use ezu_features::ops::scatter::{scatter_polygons, ScatterOpts};
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, FeatureGroup};

/// Default lattice salt. Constant rather than `EvalCtx::rng_seed` so the
/// world-anchored scatter is the same function on every tile.
const DEFAULT_SEED: u32 = 0x5C_A7_7E_12;

/// Parse an optional raw MapLibre expression field, type-checked against
/// `expect`. Returns `(parsed, raw_json_text)` for a stable cache hash.
fn parse_expr_field(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    expect: &maplibre_expr::Type,
) -> Result<(Option<maplibre_expr::Expr>, Option<String>), FactoryError> {
    match fields.get(name) {
        Some(v) => {
            let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
                field: name.into(),
                msg: e.to_string(),
            })?;
            let expr = maplibre_expr::typecheck(&expr, Some(expect), false).map_err(|e| {
                FactoryError::BadField {
                    field: name.into(),
                    msg: e.to_string(),
                }
            })?;
            Ok((Some(expr), Some(v.to_string())))
        }
        None => Ok((None, None)),
    }
}

/// Evaluate a `Number` expression for a group, falling back to `fallback`
/// when the expression is absent or doesn't resolve to a number.
fn eval_number(
    expr: &Option<maplibre_expr::Expr>,
    ectx: &maplibre_expr::EvaluationContext,
    fallback: f64,
) -> f64 {
    match expr {
        Some(e) => match maplibre_expr::evaluate(e, ectx) {
            Ok(maplibre_expr::Value::Number(n)) => n,
            _ => fallback,
        },
        None => fallback,
    }
}

struct DotDensityNode {
    density: In<f64>,
    density_expr: Option<maplibre_expr::Expr>,
    density_expr_src: Option<String>,
    dot_value: In<f64>,
    spacing_px: In<f64>,
    jitter: In<f64>,
    seed: u32,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for DotDensityNode {
    fn op_name(&self) -> &'static str {
        "dot-density"
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
        let extent = feats.extent.max(1) as f64;
        // Screen pixels to feature-extent units, as the other px-sized
        // geometry nodes do.
        let px = extent / ctx.canvas.tile_w.max(1) as f64;
        let spacing = self.spacing_px.get(ctx, inputs)? * px;
        let jitter = self.jitter.get(ctx, inputs)?;
        let dot_value = self.dot_value.get(ctx, inputs)?;
        let const_density = self.density.get(ctx, inputs)?;

        // World geometry of this tile, in feature-extent units: the
        // lattice origin, and the span the whole world covers (which is
        // what turns a world y into a Mercator latitude).
        let origin = (ctx.tile.x as f64 * extent, ctx.tile.y as f64 * extent);
        let world_span = extent * (1u64 << ctx.tile.z) as f64;
        let opts = ScatterOpts {
            spacing,
            jitter,
            origin,
            salt: self.seed,
        };
        let z = ctx.tile.z;

        let mut out_groups = Vec::with_capacity(feats.groups.len());
        for g in &feats.groups {
            let ectx = crate::render::group_expr_context(g, z);
            let density_km2 = eval_number(&self.density_expr, &ectx, const_density);
            // A non-positive dot value or density has no dots in it;
            // emit an empty group rather than diverge.
            let dots_per_km2 = if dot_value > 0.0 && density_km2 > 0.0 {
                density_km2 / dot_value
            } else {
                0.0
            };
            let points = if dots_per_km2 > 0.0 {
                scatter_polygons(&g.polygons, &opts, |wy| {
                    // Mercator's scale, and so the ground area behind one
                    // square extent unit, depends on latitude. Taking it
                    // per lattice row keeps the dot count honest without
                    // introducing a step at tile borders.
                    let m_per_unit =
                        metres_per_world_unit((wy / world_span).clamp(0.0, 1.0)) / world_span;
                    let km2_per_unit2 = (m_per_unit / 1000.0).powi(2);
                    dots_per_km2 * km2_per_unit2
                })
            } else {
                vec![]
            };
            out_groups.push(FeatureGroup {
                properties: g.properties.clone(),
                polygons: vec![],
                lines: vec![],
                points,
            });
        }
        Ok(features_value(feats.extent, out_groups))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"dot-density");
        self.density.param_hash(h);
        self.dot_value.param_hash(h);
        self.spacing_px.param_hash(h);
        self.jitter.param_hash(h);
        h.update(&self.seed.to_le_bytes());
        if let Some(s) = &self.density_expr_src {
            h.update(b"densityexpr");
            h.update(s.as_bytes());
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct DotDensityFactory;
impl NodeFactory for DotDensityFactory {
    fn op_name(&self) -> &'static str {
        "dot-density"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let (density_expr, density_expr_src) =
            parse_expr_field(fields, "density-expr", &maplibre_expr::Type::Number)?;
        let seed = fields
            .get("seed")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .unwrap_or(DEFAULT_SEED);

        let mut r = InReader::new(fields, ctx, 1);
        let density = r.number_or("density", 0.0)?;
        let dot_value = r.number_or("dot-value", 1.0)?;
        let spacing_px = r.number_or("spacing-px", 3.0)?;
        let jitter = r.number_or("jitter", 1.0)?;
        let parts = r.finish();

        // A dot worth nothing, or a lattice with no cells, has no
        // meaning. Catch the statically known cases at build time; a
        // `@node` port has no static bound, and eval emits nothing.
        for (name, field) in [("dot-value", &dot_value), ("spacing-px", &spacing_px)] {
            if let Some(b) = field.static_bound() {
                if b <= 0.0 {
                    return Err(FactoryError::BadField {
                        field: name.into(),
                        msg: format!("{name} must be > 0"),
                    });
                }
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
            node: Box::new(DotDensityNode {
                density,
                density_expr,
                density_expr_src,
                dot_value,
                spacing_px,
                jitter,
                seed,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Scatter points inside polygons at a per-feature areal density (dot density map).",
            "properties": {
                "features": schema_frag::node_ref(),
                "density": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "default": 0.0,
                              "description": "Constant density in units per square kilometre, used where `density-expr` is absent." })),
                "density-expr": { "description": "Per-feature MapLibre expression giving density in units per square kilometre, e.g. [\"/\", [\"get\", \"POP\"], [\"get\", \"AREA_KM2\"]]." },
                "dot-value": schema_frag::in_number(serde_json::json!({ "type": "number", "exclusiveMinimum": 0.0, "default": 1.0,
                                "description": "Units one dot stands for. 100 means one dot per 100 people." })),
                "spacing-px": schema_frag::in_number(serde_json::json!({ "type": "number", "exclusiveMinimum": 0.0, "default": 3.0,
                                 "description": "Lattice cell size in screen pixels. One dot per cell at most, so this caps how dense the scatter can get." })),
                "jitter": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 1.0,
                             "description": "How far a dot may stray from its cell centre, as a fraction of the cell. 0 leaves a visible grid." })),
                "seed": { "type": "integer", "minimum": 0, "description": "Lattice salt; change it to reshuffle the dots without changing their density." },
            },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(DotDensityFactory);
