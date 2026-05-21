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
    schema_frag, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, Node,
    NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::common::{features_value, read_number, read_number_or, read_optional_string};

const DEFAULT_EXTENT: u32 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    Tile,
    World,
}

struct PointGridNode {
    extent: u32,
    spacing_x: f64,
    spacing_y: f64,
    offset_x: f64,
    offset_y: f64,
    anchor: Anchor,
}

impl Node for PointGridNode {
    fn op_name(&self) -> &'static str {
        "point-grid"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self) -> PortKind {
        PortKind::Features
    }
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::Tile
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        _inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let e = self.extent as f64;
        // World origin in tile-local coords: subtract the tile's world
        // offset so a single global grid lines up across neighbours.
        let (ox, oy) = match self.anchor {
            Anchor::Tile => (self.offset_x, self.offset_y),
            Anchor::World => (
                self.offset_x - (ctx.tile.x as f64) * e,
                self.offset_y - (ctx.tile.y as f64) * e,
            ),
        };
        // Find the first grid index that lands inside [0, extent].
        let i0 = ((-ox) / self.spacing_x).ceil() as i64;
        let i1 = ((e - ox) / self.spacing_x).floor() as i64;
        let j0 = ((-oy) / self.spacing_y).ceil() as i64;
        let j1 = ((e - oy) / self.spacing_y).floor() as i64;

        let mut points = Vec::new();
        let mut j = j0;
        while j <= j1 {
            let y = oy + (j as f64) * self.spacing_y;
            let yi = y.round() as i32;
            let mut i = i0;
            while i <= i1 {
                let x = ox + (i as f64) * self.spacing_x;
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
        h.update(&self.spacing_x.to_le_bytes());
        h.update(&self.spacing_y.to_le_bytes());
        h.update(&self.offset_x.to_le_bytes());
        h.update(&self.offset_y.to_le_bytes());
        h.update(match self.anchor {
            Anchor::Tile => &[0u8],
            Anchor::World => &[1u8],
        });
    }
}

pub(super) struct PointGridFactory;
impl NodeFactory for PointGridFactory {
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
        let spacing = read_number(fields, "spacing", ctx)?;
        let spacing_x = read_number_or(fields, "spacing-x", ctx, spacing)?;
        let spacing_y = read_number_or(fields, "spacing-y", ctx, spacing)?;
        if spacing_x <= 0.0 || spacing_y <= 0.0 {
            return Err(FactoryError::BadField {
                field: "spacing".into(),
                msg: "spacing must be > 0".into(),
            });
        }
        let offset_x = read_number_or(fields, "offset-x", ctx, 0.0)?;
        let offset_y = read_number_or(fields, "offset-y", ctx, 0.0)?;
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
        Ok(BuiltNode {
            node: Box::new(PointGridNode {
                extent,
                spacing_x,
                spacing_y,
                offset_x,
                offset_y,
                anchor,
            }),
            connections: vec![],
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
                "offset-x": { "type": "number", "default": 0.0 },
                "offset-y": { "type": "number", "default": 0.0 },
                "anchor": { "type": "string", "enum": ["tile", "world"], "default": "tile" },
            },
            "required": ["spacing"],
        })
    }
}
