//! `circles` — `Features -> Raster`. Draws a filled disk at every point of
//! every feature group, with per-feature radius / color / stroke. This is the
//! crisp vector counterpart to MapLibre's `circle` layer — the sharp
//! replacement for the old sprite+`stamp` circle path.
//!
//! Every paint property is a constant `In<...>` with an optional raw-expr
//! sibling (`radius-expr`, `color-expr`, `opacity-expr`, `stroke-width-expr`,
//! `stroke-color-expr`) evaluated per feature group via `maplibre-expr`,
//! mirroring the data-driven pattern in `stroke` / `fill-solid`.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use tiny_skia::{FillRule, Paint, PathBuilder, Stroke, Transform};
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    canvas_into_raster, color_f32_to_u8, downcast_features, empty_raster, make_canvas,
    tint_alpha_color,
};

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
    fallback: f32,
) -> f32 {
    match expr {
        Some(e) => match maplibre_expr::evaluate(e, ectx) {
            Ok(maplibre_expr::Value::Number(n)) => n as f32,
            _ => fallback,
        },
        None => fallback,
    }
}

/// Evaluate a `Color` expression for a group into straight RGBA8, falling
/// back to `fallback` when absent or non-color. maplibre-expr `Color` stores
/// straight channels in `0..=1`, exactly like a parsed hex literal — so an
/// opaque data-driven color paints the same pixels as the constant `color`.
fn eval_color(
    expr: &Option<maplibre_expr::Expr>,
    ectx: &maplibre_expr::EvaluationContext,
    fallback: [u8; 4],
) -> [u8; 4] {
    match expr {
        Some(e) => match maplibre_expr::evaluate(e, ectx) {
            Ok(maplibre_expr::Value::Color(c)) => {
                color_f32_to_u8([c.r as f32, c.g as f32, c.b as f32, c.a as f32])
            }
            _ => fallback,
        },
        None => fallback,
    }
}

struct CirclesNode {
    radius: In<f64>,
    color: In<[f32; 4]>,
    opacity: In<f64>,
    stroke_width: In<f64>,
    stroke_color: In<[f32; 4]>,
    radius_expr: Option<maplibre_expr::Expr>,
    color_expr: Option<maplibre_expr::Expr>,
    opacity_expr: Option<maplibre_expr::Expr>,
    stroke_width_expr: Option<maplibre_expr::Expr>,
    stroke_color_expr: Option<maplibre_expr::Expr>,
    radius_expr_src: Option<String>,
    color_expr_src: Option<String>,
    opacity_expr_src: Option<String>,
    stroke_width_expr_src: Option<String>,
    stroke_color_expr_src: Option<String>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for CirclesNode {
    fn op_name(&self) -> &'static str {
        "circles"
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
        let feats = downcast_features(
            inputs[0]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("features".into()))?,
        )?;
        if feats.points.is_empty() {
            return Ok(empty_raster(ctx));
        }

        // Constants, resolved once. Data-driven exprs (if present) override
        // these per feature group; whichever expr is absent uses the constant.
        let const_radius = (self.radius.get(ctx, inputs)? as f32).max(0.0);
        let const_color = color_f32_to_u8(self.color.get(ctx, inputs)?);
        let const_opacity = self.opacity.get(ctx, inputs)? as f32;
        let const_stroke_width = (self.stroke_width.get(ctx, inputs)? as f32).max(0.0);
        let const_stroke_color = color_f32_to_u8(self.stroke_color.get(ctx, inputs)?);

        let mut canvas = make_canvas(ctx)?;
        let pad = canvas.pad() as f32;
        let tile_w = canvas.tile_width() as f32;
        let tile_h = canvas.tile_height() as f32;
        let extent = feats.extent.max(1) as f32;
        let sx = tile_w / extent;
        let sy = tile_h / extent;
        let z = ctx.tile.z;

        let paint_group = |canvas: &mut crate::Canvas,
                           group: &crate::nodes::common::FeatureGroup| {
            let ectx = crate::render::group_expr_context(group, z);
            let radius = eval_number(&self.radius_expr, &ectx, const_radius).max(0.0);
            if radius <= 0.0 {
                return;
            }
            let opacity = eval_number(&self.opacity_expr, &ectx, const_opacity);
            let fill_rgba = eval_color(&self.color_expr, &ectx, const_color);
            let fill_color = tint_alpha_color(fill_rgba, opacity);
            let stroke_width = eval_number(&self.stroke_width_expr, &ectx, const_stroke_width);
            let stroke_rgba = eval_color(&self.stroke_color_expr, &ectx, const_stroke_color);
            let stroke_color = tint_alpha_color(stroke_rgba, opacity);

            let pm = canvas.pixmap_mut();
            for &(x, y) in &group.points {
                let px = x as f32 * sx + pad;
                let py = y as f32 * sy + pad;
                let mut pb = PathBuilder::new();
                pb.push_circle(px, py, radius);
                let Some(path) = pb.finish() else {
                    continue;
                };
                // Fill first, stroke outline on top (matches MapLibre
                // `circle-stroke` sitting above the fill).
                let mut fill_paint = Paint::default();
                fill_paint.set_color(fill_color);
                fill_paint.anti_alias = true;
                pm.fill_path(
                    &path,
                    &fill_paint,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
                if stroke_width > 0.0 {
                    let mut sp = Paint::default();
                    sp.set_color(stroke_color);
                    sp.anti_alias = true;
                    pm.stroke_path(
                        &path,
                        &sp,
                        &Stroke {
                            width: stroke_width,
                            ..Default::default()
                        },
                        Transform::identity(),
                        None,
                    );
                }
            }
        };

        // Synthetic geometry (e.g. `literal-geometry`) carries no groups;
        // fall back to a single empty-property group over the flat points.
        if feats.groups.is_empty() {
            let synthetic = crate::nodes::common::FeatureGroup {
                properties: std::collections::HashMap::new(),
                polygons: Vec::new(),
                lines: Vec::new(),
                points: feats.points.clone(),
            };
            paint_group(&mut canvas, &synthetic);
        } else {
            for group in &feats.groups {
                paint_group(&mut canvas, group);
            }
        }

        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"circles");
        self.radius.param_hash(h);
        self.color.param_hash(h);
        self.opacity.param_hash(h);
        self.stroke_width.param_hash(h);
        self.stroke_color.param_hash(h);
        for (tag, src) in [
            (b"radiusexpr".as_slice(), &self.radius_expr_src),
            (b"colorexpr".as_slice(), &self.color_expr_src),
            (b"opacityexpr".as_slice(), &self.opacity_expr_src),
            (b"strokewidthexpr".as_slice(), &self.stroke_width_expr_src),
            (b"strokecolorexpr".as_slice(), &self.stroke_color_expr_src),
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

pub(super) struct CirclesFactory;
impl NodeFactory for CirclesFactory {
    fn op_name(&self) -> &'static str {
        "circles"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let mut r = InReader::new(fields, ctx, 1);
        let radius = r.number_or("radius", 5.0)?;
        let color = r.color_or("color", [0.0, 0.0, 0.0, 1.0])?;
        let opacity = r.number_or("opacity", 1.0)?;
        let stroke_width = r.number_or("stroke-width", 0.0)?;
        let stroke_color = r.color_or("stroke-color", [0.0, 0.0, 0.0, 1.0])?;
        let parts = r.finish();

        let (radius_expr, radius_expr_src) =
            parse_expr_field(fields, "radius-expr", &maplibre_expr::Type::Number)?;
        let (color_expr, color_expr_src) =
            parse_expr_field(fields, "color-expr", &maplibre_expr::Type::Color)?;
        let (opacity_expr, opacity_expr_src) =
            parse_expr_field(fields, "opacity-expr", &maplibre_expr::Type::Number)?;
        let (stroke_width_expr, stroke_width_expr_src) =
            parse_expr_field(fields, "stroke-width-expr", &maplibre_expr::Type::Number)?;
        let (stroke_color_expr, stroke_color_expr_src) =
            parse_expr_field(fields, "stroke-color-expr", &maplibre_expr::Type::Color)?;

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
            node: Box::new(CirclesNode {
                radius,
                color,
                opacity,
                stroke_width,
                stroke_color,
                radius_expr,
                color_expr,
                opacity_expr,
                stroke_width_expr,
                stroke_color_expr,
                radius_expr_src,
                color_expr_src,
                opacity_expr_src,
                stroke_width_expr_src,
                stroke_color_expr_src,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Filled disks at every feature point (tiny-skia), with per-feature radius, color, opacity, and optional stroke ring. The crisp vector counterpart to MapLibre's `circle` layer. Each paint property has an optional `*-expr` MapLibre-expression sibling evaluated per feature group.",
            "properties": {
                "features": schema_frag::node_ref(),
                "radius": schema_frag::px_number(),
                "radius-expr": {
                    "description": "A MapLibre number expression (px), evaluated per feature group; overrides the constant `radius`.",
                },
                "color": schema_frag::color(),
                "color-expr": {
                    "description": "A MapLibre color expression (JSON array, e.g. [\"match\", [\"get\", \"c\"], \"a\", \"#f00\", \"#0f0\"]), evaluated per feature group; overrides the constant `color`.",
                },
                "opacity": schema_frag::unit_number(),
                "opacity-expr": {
                    "description": "A MapLibre number expression giving opacity, evaluated per feature group; multiplies both fill and stroke alpha. Overrides the constant `opacity`.",
                },
                "stroke-width": schema_frag::px_number(),
                "stroke-width-expr": {
                    "description": "A MapLibre number expression (px) for the stroke ring, evaluated per feature group; overrides the constant `stroke-width`. A ring is drawn only when width > 0.",
                },
                "stroke-color": schema_frag::color(),
                "stroke-color-expr": {
                    "description": "A MapLibre color expression for the stroke ring, evaluated per feature group; overrides the constant `stroke-color`.",
                },
            },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(CirclesFactory);
