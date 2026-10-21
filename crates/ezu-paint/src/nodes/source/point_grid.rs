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
//! used downstream). The lattice covers the padded canvas, so points
//! just outside the tile exist and whatever draws them spills in.

use ezu_graph::{
    schema_frag, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, In, InEnum,
    InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{anchor_schema, features_value, Anchor, FeatureGroup, ANCHORS};

const DEFAULT_EXTENT: u32 = 4096;

struct PointGridNode {
    extent: u32,
    spacing_x: In<f64>,
    spacing_y: In<f64>,
    offset_x: In<f64>,
    offset_y: In<f64>,
    stagger: In<f64>,
    anchor: InEnum<Anchor>,
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
        let stagger = self.stagger.get(ctx, inputs)?;
        // A non-positive spacing has no lattice; emit nothing rather
        // than diverge.
        if spacing_x <= 0.0 || spacing_y <= 0.0 {
            return Ok(features_value(self.extent, vec![]));
        }
        // World origin in tile-local coords: subtract the tile's world
        // offset so a single global grid lines up across neighbours.
        let (ox, oy) = match self.anchor.get(ctx)? {
            Anchor::Tile => (offset_x, offset_y),
            Anchor::World => (
                offset_x - (ctx.tile.x as f64) * e,
                offset_y - (ctx.tile.y as f64) * e,
            ),
        };
        // Everything downstream draws on the padded canvas, and ops with an
        // extent around each point (a sprite in `stamp`, a radius in
        // `circles`) need the points just outside the tile to draw their
        // spill into it — without them every tile border shows a seam. So
        // cover the padded area, not the visible tile. The pad is in canvas
        // pixels and the lattice is in extent units; round the converted
        // margin outwards so no point in the margin is dropped.
        let margin_x = (ctx.canvas.pad as f64 * e / ctx.canvas.tile_w.max(1) as f64).ceil();
        let margin_y = (ctx.canvas.pad as f64 * e / ctx.canvas.tile_h.max(1) as f64).ceil();
        // Find the first grid index that lands inside the padded area.
        let j0 = ((-margin_y - oy) / spacing_y).ceil() as i64;
        let j1 = ((e + margin_y - oy) / spacing_y).floor() as i64;

        let mut points = Vec::new();
        let mut j = j0;
        while j <= j1 {
            let y = oy + (j as f64) * spacing_y;
            let yi = y.round() as i32;
            // Odd rows shift sideways. `j` counts from the anchor's origin,
            // so under `world` neighbouring tiles agree on which rows are odd.
            let row_x = if j.rem_euclid(2) == 1 {
                ox + stagger * spacing_x
            } else {
                ox
            };
            let i0 = ((-margin_x - row_x) / spacing_x).ceil() as i64;
            let i1 = ((e + margin_x - row_x) / spacing_x).floor() as i64;
            let mut i = i0;
            while i <= i1 {
                let x = row_x + (i as f64) * spacing_x;
                points.push((x.round() as i32, yi));
                i += 1;
            }
            j += 1;
        }
        Ok(features_value(
            self.extent,
            vec![FeatureGroup::synthetic(vec![], vec![], points)],
        ))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"point-grid");
        h.update(&self.extent.to_le_bytes());
        self.spacing_x.param_hash(h);
        self.spacing_y.param_hash(h);
        self.offset_x.param_hash(h);
        self.offset_y.param_hash(h);
        self.stagger.param_hash(h);
        self.anchor.param_hash(h);
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
        let mut r = InReader::new(fields, ctx, 0);
        let anchor = r.enum_or("anchor", Anchor::Tile, ANCHORS)?;
        let (spacing_x, spacing_y) = super::read_spacing(&mut r, fields)?;
        let offset_x = r.number_or("offset-x", 0.0)?;
        let offset_y = r.number_or("offset-y", 0.0)?;
        let stagger = r.number_or("stagger", 0.0)?;
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
                stagger,
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
                "spacing": schema_frag::in_number(serde_json::json!({
                    "type": "number", "minimum": 0.0,
                    "description": "Grid pitch in *extent* units, not pixels — `extent` below is the coordinate space (4096 by default), so a 512 px tile fits 8 points at `spacing: 512`. Paint ops downstream take pixels, so the two do not match. Required unless both `spacing-x` and `spacing-y` are given."
                })),
                "spacing-x": super::spacing_axis_schema("x"),
                "spacing-y": super::spacing_axis_schema("y"),
                "offset-x": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.0 })),
                "offset-y": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.0 })),
                "stagger": schema_frag::in_number(serde_json::json!({
                    "type": "number", "default": 0.0,
                    "description": "Shift every other row sideways by this fraction of `spacing-x`. `0.5` puts each row's points halfway between the row above's — the centres of a running brick bond, or a triangular lattice."
                })),
                "anchor": anchor_schema("tile", None),
            },
            "anyOf": super::spacing_any_of(),
        })
    }
}

ezu_graph::submit_node!(PointGridFactory);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::geometry::ratio_test::{build, eval, plain, retina};

    #[test]
    fn a_world_grid_lands_on_the_same_points_at_every_pixel_ratio() {
        let node = build(
            &PointGridFactory,
            serde_json::json!({ "spacing": 300, "offset-x": 17, "stagger": 0.5, "anchor": "world" }),
        );
        let one = eval(node.as_ref(), plain(), &[]);
        let two = eval(node.as_ref(), retina(), &[]);
        let pts: Vec<_> = one.points().collect();
        assert!(pts.len() > 100, "a lattice covers the tile");
        assert_eq!(pts, two.points().collect::<Vec<_>>());
    }
}
