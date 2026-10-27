//! `transform` — `Features -> Features`. Apply an affine (translate +
//! rotate + scale) transform to every vertex. Rotation happens around
//! `pivot` (in feature-space coordinates, default = origin) before
//! the final translation. Scale is per-axis.
//!
//! The scalar forms — `translate-x` / `translate-y`, `rotation-deg`,
//! `scale` / `scale-x` / `scale-y` — are `In<f64>` fields, so they take a
//! literal, a `$param` a caller overrides per render, or an `@node`
//! scalar port. Nothing here decides canvas padding, so there is no
//! static-bound requirement. The `[x, y]` array forms (`translate`,
//! `scale-xy`, `pivot`) stay literal: an array is not a scalar, so a
//! param cannot stand in for one.
//!
//! `translate-x-px` / `translate-y-px` (seeded by the literal
//! `translate-px`) shift by screen pixels instead, the unit MapLibre's
//! `*-translate` properties use, converted to feature units for the tile
//! being drawn so the shift looks the same at every tile size and extent.

use ezu_features::ops::transform::transform;
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, InfluenceCtx, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, map_with_neighbours, read_xy, FeatureGroup};

struct TransformNode {
    translate_x: In<f64>,
    translate_y: In<f64>,
    /// Shift in canvas pixels, added after `translate-x` / `translate-y`.
    translate_x_px: In<f64>,
    translate_y_px: In<f64>,
    rotation_deg: In<f64>,
    scale: In<f64>,
    scale_x: In<f64>,
    scale_y: In<f64>,
    pivot: (f64, f64),
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for TransformNode {
    fn op_name(&self) -> &'static str {
        "transform"
    }

    /// Nothing upstream of this op may be dropped.
    ///
    /// `translate-*` moves geometry by a distance with no declared
    /// ceiling, so nothing upstream can be ruled out.
    fn influence_pad(&self, _ctx: &InfluenceCtx<'_>) -> u32 {
        InfluenceCtx::UNBOUNDED
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
        // Style pixels to feature units: into device pixels by the pixel
        // ratio, then one tile spans `extent` units across `tile_w` device
        // px (and down `tile_h`).
        map_with_neighbours(ctx, &feats, |ctx, extent, groups| {
            let extent = extent as f64;
            let translate = (
                self.translate_x.get(ctx, inputs)?
                    + ctx.px(self.translate_x_px.get(ctx, inputs)?) * extent
                        / ctx.canvas.tile_w.max(1) as f64,
                self.translate_y.get(ctx, inputs)?
                    + ctx.px(self.translate_y_px.get(ctx, inputs)?) * extent
                        / ctx.canvas.tile_h.max(1) as f64,
            );
            let rotation_rad = self.rotation_deg.get(ctx, inputs)?.to_radians();
            let uniform = self.scale.get(ctx, inputs)?;
            let scale = (
                uniform * self.scale_x.get(ctx, inputs)?,
                uniform * self.scale_y.get(ctx, inputs)?,
            );
            // Per group: apply the affine to each feature's vertices,
            // carrying properties.
            let mut out_groups = Vec::with_capacity(groups.len());
            for g in groups {
                let mut points = g.points.clone();
                let mut lines = g.lines.clone();
                let mut polygons = g.polygons.clone();
                transform(
                    &mut points,
                    &mut lines,
                    &mut polygons,
                    scale,
                    rotation_rad,
                    self.pivot,
                    translate,
                );
                out_groups.push(FeatureGroup {
                    properties: g.properties.clone(),
                    polygons,
                    lines,
                    points,
                });
            }
            Ok(out_groups)
        })
    }
    fn forwards_neighbours(&self, port: usize) -> bool {
        port == 0
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"transform");
        self.translate_x.param_hash(h);
        self.translate_y.param_hash(h);
        self.translate_x_px.param_hash(h);
        self.translate_y_px.param_hash(h);
        self.rotation_deg.param_hash(h);
        self.scale.param_hash(h);
        self.scale_x.param_hash(h);
        self.scale_y.param_hash(h);
        h.update(&self.pivot.0.to_le_bytes());
        h.update(&self.pivot.1.to_le_bytes());
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct TransformFactory;
impl NodeFactory for TransformFactory {
    fn op_name(&self) -> &'static str {
        "transform"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        // The `[x, y]` arrays are literal-only, and seed the per-axis
        // scalars that a caller can drive per render.
        let tx = read_xy(fields, "translate", ctx, [0.0, 0.0])?;
        let pivot = read_xy(fields, "pivot", ctx, [0.0, 0.0])?;

        let mut r = InReader::new(fields, ctx, 1);
        let translate_x = r.number_or("translate-x", tx[0] as f64)?;
        let translate_y = r.number_or("translate-y", tx[1] as f64)?;
        let rotation_deg = r.number_or("rotation-deg", 0.0)?;
        // Uniform `scale` multiplies the per-axis factors, and the literal
        // `scale-xy` array seeds those factors. All three default to 1, so
        // `scale: 0.5` and `scale-xy: [2, 1]` mean exactly what they did
        // before, and `scale-x: "$sx"` now works alongside them.
        let scale_xy = read_xy(fields, "scale-xy", ctx, [1.0, 1.0])?;
        let scale = r.number_or("scale", 1.0)?;
        let scale_x = r.number_or("scale-x", scale_xy[0] as f64)?;
        let scale_y = r.number_or("scale-y", scale_xy[1] as f64)?;
        let tx_px = read_xy(fields, "translate-px", ctx, [0.0, 0.0])?;
        let translate_x_px = r.number_or("translate-x-px", tx_px[0] as f64)?;
        let translate_y_px = r.number_or("translate-y-px", tx_px[1] as f64)?;
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
            node: Box::new(TransformNode {
                translate_x,
                translate_y,
                translate_x_px,
                translate_y_px,
                rotation_deg,
                scale,
                scale_x,
                scale_y,
                pivot: (pivot[0] as f64, pivot[1] as f64),
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Translate / rotate / scale every input vertex. Rotation happens around `pivot` (defaults to the origin) before the final translation. The scalar fields take a `$param` or an `@node` port and follow it per render; the `[x, y]` arrays are literal, and seed the matching scalars. Uniform `scale` multiplies the per-axis factors.",
            "properties": {
                "features": schema_frag::node_ref(),
                "translate": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "Literal [x, y] shift. `translate-x` / `translate-y` override it." },
                "translate-x": schema_frag::number(),
                "translate-y": schema_frag::number(),
                "translate-px": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "Literal [x, y] shift in pixels, right and down, as MapLibre's `circle-translate` / `fill-translate` / `line-translate` give it; converted to feature units for the tile being drawn, so the shift looks the same at every tile size. Added after `translate`. `translate-x-px` / `translate-y-px` override it." },
                "translate-x-px": schema_frag::number(),
                "translate-y-px": schema_frag::number(),
                "rotation-deg": schema_frag::number(),
                "scale": schema_frag::number(),
                "scale-xy": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "Literal per-axis scale. `scale-x` / `scale-y` override it; uniform `scale` multiplies it." },
                "scale-x": schema_frag::number(),
                "scale-y": schema_frag::number(),
                "pivot": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "Literal [x, y] rotation centre in feature space." },
            },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(TransformFactory);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::geometry::ratio_test::{build, eval, input, plain, retina};

    #[test]
    fn a_pixel_translate_moves_the_same_distance_on_the_map_at_every_ratio() {
        let node = build(
            &TransformFactory,
            serde_json::json!({ "features": "@f", "translate-px": [10, -5], "translate": [3, 0] }),
        );
        let f = input(FeatureGroup::synthetic(vec![], vec![], vec![(100, 200)]));
        let one = eval(node.as_ref(), plain(), std::slice::from_ref(&f));
        let two = eval(node.as_ref(), retina(), std::slice::from_ref(&f));
        // 10 style px is 160 extent units on either tile: 10 device px of
        // 16 units on the 256 px tile, 20 device px of 8 units on the
        // 512 px one. The extent-unit `translate` is not a pixel length.
        assert_eq!(one.points().collect::<Vec<_>>(), [(263, 120)]);
        assert_eq!(two.points().collect::<Vec<_>>(), [(263, 120)]);
    }
}
