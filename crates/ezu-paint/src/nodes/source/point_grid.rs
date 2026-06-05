//! `point-grid` — `() -> Features`. Regular lattice of points covering
//! the current tile.
//!
//! Two anchor modes:
//!
//! - `tile` — grid origin is the tile's `(0, 0)` corner. Each tile gets
//!   the same local layout regardless of zoom or position.
//! - `world` — grid origin is global `(0, 0)`. Adjacent tiles share
//!   lattice points, producing seamless dot patterns across tile seams.
//!
//! Spacing and offsets are in tile-local pixel units (the same `extent`
//! used downstream).

use ezu_graph::{
    schema_frag, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, In, InReader,
    Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{features_value, read_number, read_optional_string};

const DEFAULT_EXTENT: u32 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    Tile,
    World,
}

struct PointGridNode {
    extent: u32,
    spacing_x: In<f64>,
    spacing_y: In<f64>,
    offset_x: In<f64>,
    offset_y: In<f64>,
    anchor: Anchor,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for PointGridNode {
    fn op_name(&self) -> &'static str {
        "point-grid"
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
        let e = self.extent as f64;
        let spacing_x = self.spacing_x.get(ctx, inputs)?;
        let spacing_y = self.spacing_y.get(ctx, inputs)?;
        let offset_x = self.offset_x.get(ctx, inputs)?;
        let offset_y = self.offset_y.get(ctx, inputs)?;
        // A non-positive spacing has no lattice; emit nothing rather
        // than diverge.
        if spacing_x <= 0.0 || spacing_y <= 0.0 {
            return Ok(features_value(self.extent, vec![], vec![], vec![]));
        }
        // World origin in tile-local coords: subtract the tile's world
        // offset so a single global grid lines up across neighbours.
        let (ox, oy) = match self.anchor {
            Anchor::Tile => (offset_x, offset_y),
            Anchor::World => (
                offset_x - (ctx.tile.x as f64) * e,
                offset_y - (ctx.tile.y as f64) * e,
            ),
        };
        // Find the first grid index that lands inside [0, extent].
        let i0 = ((-ox) / spacing_x).ceil() as i64;
        let i1 = ((e - ox) / spacing_x).floor() as i64;
        let j0 = ((-oy) / spacing_y).ceil() as i64;
        let j1 = ((e - oy) / spacing_y).floor() as i64;

        let mut points = Vec::new();
        let mut j = j0;
        while j <= j1 {
            let y = oy + (j as f64) * spacing_y;
            let yi = y.round() as i32;
            let mut i = i0;
            while i <= i1 {
                let x = ox + (i as f64) * spacing_x;
                points.push((x.round() as i32, yi));
                i += 1;
            }
            j += 1;
        }
        Ok(features_value(self.extent, vec![], vec![], points))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"point-grid");
        h.update(&self.extent.to_le_bytes());
        self.spacing_x.param_hash(h);
        self.spacing_y.param_hash(h);
        self.offset_x.param_hash(h);
        self.offset_y.param_hash(h);
        h.update(match self.anchor {
            Anchor::Tile => &[0u8],
            Anchor::World => &[1u8],
        });
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct PointGridFactory;
impl NodeFactory for PointGridFactory {
    fn op_name(&self) -> &'static str {
        "point-grid"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let extent = fields
            .get("extent")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .unwrap_or(DEFAULT_EXTENT);
        // `spacing` is the build-time default for the per-axis spacings;
        // it is never stored on the node, so it stays a static literal.
        let spacing = read_number(fields, "spacing", ctx)?;
        let anchor = match read_optional_string(fields, "anchor")?.as_deref() {
            None | Some("tile") => Anchor::Tile,
            Some("world") => Anchor::World,
            Some(other) => {
                return Err(FactoryError::BadField {
                    field: "anchor".into(),
                    msg: format!("unknown anchor '{other}', expected tile/world"),
                });
            }
        };

        let mut r = InReader::new(fields, ctx, 0);
        let spacing_x = r.number_or("spacing-x", spacing)?;
        let spacing_y = r.number_or("spacing-y", spacing)?;
        let offset_x = r.number_or("offset-x", 0.0)?;
        let offset_y = r.number_or("offset-y", 0.0)?;
        let parts = r.finish();

        // Spacing must be > 0; check the static bounds (literal, or a
        // `$param`'s declared `max`). A `@node` port has no static bound —
        // eval emits an empty lattice for non-positive values instead.
        for (name, sp) in [("spacing-x", &spacing_x), ("spacing-y", &spacing_y)] {
            if let Some(b) = sp.static_bound() {
                if b <= 0.0 {
                    return Err(FactoryError::BadField {
                        field: name.into(),
                        msg: "spacing must be > 0".into(),
                    });
                }
            }
        }

        Ok(BuiltNode {
            node: Box::new(PointGridNode {
                extent,
                spacing_x,
                spacing_y,
                offset_x,
                offset_y,
                anchor,
                ports: parts.ports,
                param_refs: parts.param_refs,
            }),
            connections: parts.connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Regular lattice of points covering the tile.",
            "properties": {
                "extent": { "type": "integer", "minimum": 1, "default": DEFAULT_EXTENT },
                "spacing": schema_frag::px_number(),
                "spacing-x": schema_frag::px_number(),
                "spacing-y": schema_frag::px_number(),
                "offset-x": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.0 })),
                "offset-y": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.0 })),
                "anchor": { "type": "string", "enum": ["tile", "world"], "default": "tile" },
            },
            "required": ["spacing"],
        })
    }
}

ezu_graph::submit_node!(PointGridFactory);
