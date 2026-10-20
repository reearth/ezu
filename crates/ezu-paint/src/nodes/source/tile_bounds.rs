//! `tile-bounds` — `() -> Features`. Emits the tile's full
//! `[0, extent] × [0, extent]` rectangle as a single polygon. Useful
//! as a base for `fill-solid` (full-tile background), `hatch` (full-
//! tile pattern), or as a mask source.
//!
//! `cover: "canvas"` grows the rectangle over the padded canvas instead.
//! Geometry built from the visible tile alone stops at its edge, so
//! anything that reaches across the border — a Voronoi cell, a hatch
//! line under a `warp` — comes out cut there; covering the margin as
//! `point-grid` and `point-scatter` already do lets both neighbours
//! build the same shape.

use ezu_features::Polygon;
use ezu_graph::{
    schema_frag, BuiltNode, CoordSpace, EnumTable, EvalCtx, EvalError, FactoryCtx, FactoryError,
    InEnum, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{features_value, FeatureGroup};

const DEFAULT_EXTENT: u32 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cover {
    Tile,
    Canvas,
}

const COVERS: EnumTable<Cover> = &[("tile", Cover::Tile), ("canvas", Cover::Canvas)];

struct TileBoundsNode {
    extent: u32,
    cover: InEnum<Cover>,
    param_refs: Vec<String>,
}

impl Node for TileBoundsNode {
    fn op_name(&self) -> &'static str {
        "tile-bounds"
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
        ctx: &EvalCtx<'_>,
        _inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let e = self.extent as i32;
        // The pad is in canvas pixels and the rectangle in extent units;
        // round the margin outwards so the whole padded canvas is covered.
        let (mx, my) = match self.cover.get(ctx)? {
            Cover::Tile => (0, 0),
            Cover::Canvas => {
                let pad = ctx.canvas.pad as f64 * self.extent as f64;
                (
                    (pad / ctx.canvas.tile_w.max(1) as f64).ceil() as i32,
                    (pad / ctx.canvas.tile_h.max(1) as f64).ceil() as i32,
                )
            }
        };
        let (x0, y0, x1, y1) = (-mx, -my, e + mx, e + my);
        let poly = Polygon {
            exterior: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)],
            holes: vec![],
        };
        Ok(features_value(
            self.extent,
            vec![FeatureGroup::synthetic(vec![poly], vec![], vec![])],
        ))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"tile-bounds");
        h.update(&self.extent.to_le_bytes());
        self.cover.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct TileBoundsFactory;
impl NodeFactory for TileBoundsFactory {
    fn op_name(&self) -> &'static str {
        "tile-bounds"
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
        let mut r = InReader::new(fields, ctx, 0);
        let cover = r.enum_or("cover", Cover::Tile, COVERS)?;
        Ok(BuiltNode {
            node: Box::new(TileBoundsNode {
                extent,
                cover,
                param_refs: r.finish().param_refs,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Tile-filling rectangle polygon source. `cover: canvas` grows it over the padded canvas, so geometry built from it — Voronoi cells, hatch lines — reaches past the tile's edge and neighbouring tiles build the same shapes along their shared border.",
            "properties": {
                "extent": { "type": "integer", "minimum": 1, "default": DEFAULT_EXTENT },
                "cover": schema_frag::in_enum(serde_json::json!({
                    "type": "string",
                    "enum": ["tile", "canvas"],
                    "default": "tile",
                    "description": "`tile` is the visible tile exactly; `canvas` adds the canvas padding on every side."
                })),
            },
        })
    }
}

ezu_graph::submit_node!(TileBoundsFactory);
