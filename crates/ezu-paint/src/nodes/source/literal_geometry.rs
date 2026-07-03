//! `literal-geometry` — `() -> Features`. Emits geometry from numeric
//! arrays in the style config, with no upstream feature source. Useful
//! for hard-coded decorations, debug overlays, or test fixtures.
//!
//! All three layers (points, lines, polygons) are optional; the node
//! emits whatever the config supplies. Coordinates are tile-local
//! pixels in `[0, extent]`.

use ezu_features::Polygon;
use ezu_graph::{
    BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{features_value, FeatureGroup};

const DEFAULT_EXTENT: u32 = 4096;

struct LiteralGeometryNode {
    extent: u32,
    points: Vec<(i32, i32)>,
    lines: Vec<Vec<(i32, i32)>>,
    polygons: Vec<Polygon>,
}

impl Node for LiteralGeometryNode {
    fn op_name(&self) -> &'static str {
        "literal-geometry"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
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
        _inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        // A synthetic source emits a single group with no per-feature
        // properties (any data-driven paint downstream just sees no props).
        Ok(features_value(
            self.extent,
            vec![FeatureGroup::synthetic(
                self.polygons.clone(),
                self.lines.clone(),
                self.points.clone(),
            )],
        ))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"literal-geometry");
        h.update(&self.extent.to_le_bytes());
        h.update(&(self.points.len() as u32).to_le_bytes());
        for &(x, y) in &self.points {
            h.update(&x.to_le_bytes());
            h.update(&y.to_le_bytes());
        }
        h.update(&(self.lines.len() as u32).to_le_bytes());
        for line in &self.lines {
            h.update(&(line.len() as u32).to_le_bytes());
            for &(x, y) in line {
                h.update(&x.to_le_bytes());
                h.update(&y.to_le_bytes());
            }
        }
        h.update(&(self.polygons.len() as u32).to_le_bytes());
        for p in &self.polygons {
            hash_ring(h, &p.exterior);
            h.update(&(p.holes.len() as u32).to_le_bytes());
            for hole in &p.holes {
                hash_ring(h, hole);
            }
        }
    }
}

fn hash_ring(h: &mut Xxh3, ring: &[(i32, i32)]) {
    h.update(&(ring.len() as u32).to_le_bytes());
    for &(x, y) in ring {
        h.update(&x.to_le_bytes());
        h.update(&y.to_le_bytes());
    }
}

fn parse_point(v: &Value, ctx: &'static str) -> Result<(i32, i32), FactoryError> {
    let arr = v.as_array().ok_or_else(|| FactoryError::BadField {
        field: ctx.into(),
        msg: "expected [x, y]".into(),
    })?;
    if arr.len() != 2 {
        return Err(FactoryError::BadField {
            field: ctx.into(),
            msg: format!("expected [x, y] (2 numbers), got {} elements", arr.len()),
        });
    }
    let x = arr[0].as_f64().ok_or_else(|| FactoryError::BadField {
        field: ctx.into(),
        msg: "x not a number".into(),
    })?;
    let y = arr[1].as_f64().ok_or_else(|| FactoryError::BadField {
        field: ctx.into(),
        msg: "y not a number".into(),
    })?;
    Ok((x.round() as i32, y.round() as i32))
}

fn parse_ring(v: &Value, ctx: &'static str) -> Result<Vec<(i32, i32)>, FactoryError> {
    let arr = v.as_array().ok_or_else(|| FactoryError::BadField {
        field: ctx.into(),
        msg: "expected [[x, y], ...]".into(),
    })?;
    arr.iter().map(|p| parse_point(p, ctx)).collect()
}

pub(super) struct LiteralGeometryFactory;
impl NodeFactory for LiteralGeometryFactory {
    fn op_name(&self) -> &'static str {
        "literal-geometry"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let extent = fields
            .get("extent")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .unwrap_or(DEFAULT_EXTENT);

        let points = match fields.get("points") {
            None => Vec::new(),
            Some(v) => {
                let arr = v.as_array().ok_or_else(|| FactoryError::BadField {
                    field: "points".into(),
                    msg: "expected array of [x, y]".into(),
                })?;
                arr.iter()
                    .map(|p| parse_point(p, "points"))
                    .collect::<Result<_, _>>()?
            }
        };

        let lines = match fields.get("lines") {
            None => Vec::new(),
            Some(v) => {
                let arr = v.as_array().ok_or_else(|| FactoryError::BadField {
                    field: "lines".into(),
                    msg: "expected array of polylines".into(),
                })?;
                arr.iter()
                    .map(|l| parse_ring(l, "lines"))
                    .collect::<Result<_, _>>()?
            }
        };

        let polygons = match fields.get("polygons") {
            None => Vec::new(),
            Some(v) => {
                let arr = v.as_array().ok_or_else(|| FactoryError::BadField {
                    field: "polygons".into(),
                    msg: "expected array of polygon objects".into(),
                })?;
                let mut out = Vec::with_capacity(arr.len());
                for p in arr {
                    let obj = p.as_object().ok_or_else(|| FactoryError::BadField {
                        field: "polygons".into(),
                        msg: "expected object with `exterior` and optional `holes`".into(),
                    })?;
                    let exterior = obj
                        .get("exterior")
                        .ok_or_else(|| FactoryError::MissingField("polygons[].exterior".into()))
                        .and_then(|v| parse_ring(v, "polygons.exterior"))?;
                    let holes = match obj.get("holes") {
                        None => Vec::new(),
                        Some(hv) => {
                            let harr = hv.as_array().ok_or_else(|| FactoryError::BadField {
                                field: "polygons.holes".into(),
                                msg: "expected array of rings".into(),
                            })?;
                            harr.iter()
                                .map(|h| parse_ring(h, "polygons.holes"))
                                .collect::<Result<_, _>>()?
                        }
                    };
                    out.push(Polygon { exterior, holes });
                }
                out
            }
        };

        Ok(BuiltNode {
            node: Box::new(LiteralGeometryNode {
                extent,
                points,
                lines,
                polygons,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        let pt = serde_json::json!({
            "type": "array",
            "items": { "type": "number" },
            "minItems": 2,
            "maxItems": 2,
        });
        let ring = serde_json::json!({
            "type": "array",
            "items": pt.clone(),
        });
        serde_json::json!({
            "description": "Literal geometry source. Coordinates are tile-local pixels in [0, extent].",
            "properties": {
                "extent": { "type": "integer", "minimum": 1, "default": DEFAULT_EXTENT },
                "points": { "type": "array", "items": pt },
                "lines": { "type": "array", "items": ring.clone() },
                "polygons": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "exterior": ring.clone(),
                            "holes": { "type": "array", "items": ring },
                        },
                        "required": ["exterior"],
                    },
                },
            },
        })
    }
}

ezu_graph::submit_node!(LiteralGeometryFactory);
