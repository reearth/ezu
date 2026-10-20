//! `fill-dabs` — `Features -> Raster`. Wraps
//! [`paint_polygons_dabs`](crate::paint_polygons_dabs): watercolor
//! scatter-dab fill with world-deterministic jitter (seamless across
//! tiles).

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, InfluenceCtx, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use hokusai::color::RgbaF32;
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::stamp::{eval_color_f32, eval_number, parse_expr_field};
use crate::fill_dabs::{paint_polygon_groups_dabs, DabGroup};
use crate::nodes::common::{
    canvas_into_raster, core_tile, downcast_features, empty_raster, make_canvas,
    srgb_to_linear_rgba,
};
use crate::{paint_polygons_dabs, DabFillStyle};

struct FillDabsNode {
    color: In<[f32; 4]>,
    opacity: In<f64>,
    radius_px: In<f64>,
    hardness: In<f64>,
    paint: In<f64>,
    spacing_px: In<f64>,
    position_jitter: In<f64>,
    size_jitter: In<f64>,
    opacity_jitter: In<f64>,
    value_jitter: In<f64>,
    /// Optional data-driven color / opacity: MapLibre expressions evaluated
    /// per feature group. When set, each overrides its constant counterpart
    /// for the dabs that land in the group's polygons.
    color_expr: Option<maplibre_expr::Expr>,
    opacity_expr: Option<maplibre_expr::Expr>,
    /// Raw `*-expr` JSON text, for a stable hash.
    color_expr_src: Option<String>,
    opacity_expr_src: Option<String>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for FillDabsNode {
    fn op_name(&self) -> &'static str {
        "fill-dabs"
    }

    /// Nothing upstream of this op may be dropped.
    ///
    /// A dab is painted around a centre that is only tested against
    /// the polygon, and `radius-px` has no declared ceiling.
    fn influence_pad(&self, _ctx: &InfluenceCtx<'_>) -> u32 {
        InfluenceCtx::UNBOUNDED
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::World
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let feats = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("features".into()))?;
        let feats = downcast_features(feats)?;
        if !feats.has_polygons() {
            return Ok(empty_raster(ctx));
        }
        let srgb = self.color.get(ctx, inputs)?;
        let color = srgb_to_linear_rgba(srgb);
        let mut canvas = make_canvas(ctx)?;
        let style = DabFillStyle {
            color: RgbaF32::new(color[0], color[1], color[2], 1.0),
            opacity: self.opacity.get(ctx, inputs)? as f32,
            radius_px: self.radius_px.get(ctx, inputs)? as f32,
            hardness: self.hardness.get(ctx, inputs)? as f32,
            paint: self.paint.get(ctx, inputs)? as f32,
            spacing_px: self.spacing_px.get(ctx, inputs)? as f32,
            position_jitter: self.position_jitter.get(ctx, inputs)? as f32,
            size_jitter: self.size_jitter.get(ctx, inputs)? as f32,
            opacity_jitter: self.opacity_jitter.get(ctx, inputs)? as f32,
            value_jitter: self.value_jitter.get(ctx, inputs)? as f32,
        };

        if self.color_expr.is_some() || self.opacity_expr.is_some() {
            // Data-driven fill: resolve each group's color and opacity; the
            // lattice and its jitter stay keyed by world cell, so only the
            // paint of the dabs changes. Whichever expression is absent (or
            // doesn't resolve for a group) uses the constant.
            let z = ctx.tile.z;
            let mut groups = Vec::with_capacity(feats.groups.len());
            for group in &feats.groups {
                if group.polygons.is_empty() {
                    continue;
                }
                let ectx = crate::render::group_expr_context(group, z);
                // The color's alpha is ignored, as the constant `color`'s
                // is: `opacity` carries the dabs' alpha.
                let lin = srgb_to_linear_rgba(eval_color_f32(&self.color_expr, &ectx, srgb));
                groups.push(DabGroup {
                    polygons: &group.polygons,
                    color: RgbaF32::new(lin[0], lin[1], lin[2], 1.0),
                    opacity: eval_number(&self.opacity_expr, &ectx, style.opacity),
                });
            }
            paint_polygon_groups_dabs(&mut canvas, &groups, feats.extent, core_tile(ctx), &style);
            return Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))));
        }

        let polygons: Vec<_> = feats.polygons().cloned().collect();
        paint_polygons_dabs(&mut canvas, &polygons, feats.extent, core_tile(ctx), &style);
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"fill-dabs");
        self.color.param_hash(h);
        self.opacity.param_hash(h);
        self.radius_px.param_hash(h);
        self.hardness.param_hash(h);
        self.paint.param_hash(h);
        self.spacing_px.param_hash(h);
        self.position_jitter.param_hash(h);
        self.size_jitter.param_hash(h);
        self.opacity_jitter.param_hash(h);
        self.value_jitter.param_hash(h);
        for (tag, src) in [
            (b"colorexpr".as_slice(), &self.color_expr_src),
            (b"opacityexpr".as_slice(), &self.opacity_expr_src),
        ] {
            if let Some(s) = src {
                h.update(tag);
                h.update(s.as_bytes());
            }
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct FillDabsFactory;
impl NodeFactory for FillDabsFactory {
    fn op_name(&self) -> &'static str {
        "fill-dabs"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let mut r = InReader::new(fields, ctx, 1);
        let color = r.color("color")?;
        let opacity = r.number("opacity")?;
        let radius_px = r.number("radius-px")?;
        let hardness = r.number_or("hardness", 0.5)?;
        let paint = r.number_or("paint", 1.0)?;
        let spacing_px = r.number("spacing-px")?;
        let position_jitter = r.number_or("position-jitter", 0.9)?;
        let size_jitter = r.number_or("size-jitter", 0.0)?;
        let opacity_jitter = r.number_or("opacity-jitter", 0.0)?;
        let value_jitter = r.number_or("value-jitter", 0.0)?;
        let parts = r.finish();

        let (color_expr, color_expr_src) =
            parse_expr_field(fields, "color-expr", &maplibre_expr::Type::Color)?;
        let (opacity_expr, opacity_expr_src) =
            parse_expr_field(fields, "opacity-expr", &maplibre_expr::Type::Number)?;

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
            node: Box::new(FillDabsNode {
                color,
                opacity,
                radius_px,
                hardness,
                paint,
                spacing_px,
                position_jitter,
                size_jitter,
                opacity_jitter,
                value_jitter,
                color_expr,
                opacity_expr,
                color_expr_src,
                opacity_expr_src,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Watercolor scatter-dab fill with world-deterministic jitter (seamless across tiles). Color and opacity each have an optional `*-expr` MapLibre-expression sibling evaluated per feature group; the dabs land and jitter where they would without it, and each takes the paint of the group whose polygon covers its centre.",
            "properties": {
                "features": schema_frag::node_ref(),
                "color": schema_frag::color(),
                "color-expr": {
                    "description": "A MapLibre color expression (JSON array, e.g. [\"match\", [\"get\", \"class\"], \"water\", \"#4a6c99\", \"#8a9a5b\"]), evaluated per feature group; overrides the constant `color`. As with `color`, the alpha is ignored — `opacity` or `opacity-expr` sets the dabs' opacity. A group whose expression doesn't resolve to a color falls back to `color`.",
                },
                "opacity": schema_frag::unit_number(),
                "opacity-expr": {
                    "description": "A MapLibre number expression giving the per-dab opacity, evaluated per feature group; overrides the constant `opacity`. A group whose expression doesn't resolve to a number falls back to `opacity`.",
                },
                "radius-px": schema_frag::px_number(),
                "hardness": schema_frag::unit_number(),
                "paint": schema_frag::unit_number(),
                "spacing-px": schema_frag::px_number(),
                "position-jitter": schema_frag::unit_number(),
                "size-jitter": schema_frag::unit_number(),
                "opacity-jitter": schema_frag::unit_number(),
                "value-jitter": schema_frag::unit_number(),
            },
            "required": ["features", "color", "opacity", "radius-px", "spacing-px"],
        })
    }
}

ezu_graph::submit_node!(FillDabsFactory);
