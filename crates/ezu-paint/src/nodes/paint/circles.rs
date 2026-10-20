//! `circles` — `Features -> Raster`. Draws a filled disk at every point of
//! every feature group, with per-feature radius / color / stroke. This is the
//! crisp vector counterpart to MapLibre's `circle` layer — the sharp
//! replacement for the old sprite+`stamp` circle path.
//!
//! Every paint property is a constant `In<...>` with an optional raw-expr
//! sibling (`radius-expr`, `color-expr`, `opacity-expr`, `stroke-width-expr`,
//! `stroke-color-expr`, `stroke-opacity-expr`, `blur-expr`) evaluated per
//! feature group via `maplibre-expr`,
//! mirroring the data-driven pattern in `stroke` / `fill-solid`.

use std::sync::Arc;

use ezu_graph::{
    enum_names, schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EnumTable, EvalCtx,
    EvalError, FactoryCtx, FactoryError, In, InEnum, InReader, InfluenceCtx, Node, NodeFactory,
    PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use tiny_skia::{
    FillRule, GradientStop, Paint, PathBuilder, RadialGradient, SpreadMode, Stroke, Transform,
};
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

/// Where the stroke ring sits against the circle's `radius`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum StrokeAlign {
    /// Straddling the radius, half the ring inside it and half outside.
    Center,
    /// Wholly outside the radius, as MapLibre draws `circle-stroke-width`:
    /// the fill keeps its full radius and the ring adds to it.
    Outside,
    /// Wholly inside the radius, so the circle never grows past it.
    Inside,
}

const STROKE_ALIGNS: EnumTable<StrokeAlign> = &[
    ("center", StrokeAlign::Center),
    ("outside", StrokeAlign::Outside),
    ("inside", StrokeAlign::Inside),
];

impl StrokeAlign {
    /// The radius the ring's centreline runs along, the width it is
    /// stroked at, and how far out the circle as a whole reaches, for a
    /// fill of `radius` and a ring of `width`.
    fn ring(self, radius: f32, width: f32) -> (f32, f32, f32) {
        match self {
            StrokeAlign::Center => (radius, width, radius + 0.5 * width),
            StrokeAlign::Outside => (radius + 0.5 * width, width, radius + width),
            // A ring at least as wide as the radius covers the whole disk;
            // stroking it any wider would spill past the radius.
            StrokeAlign::Inside => {
                let width = width.min(radius);
                (radius - 0.5 * width, width, radius)
            }
        }
    }
}

/// Gradient stops sampled along the blur's fade; enough that the smoothstep
/// reads as a curve rather than a few bands.
const BLUR_STOPS: usize = 9;

/// Give `paint` the circle's `color`, faded out towards `outer` (its edge,
/// centred on `centre`) when `blur` is above zero.
///
/// The fade is MapLibre's `circle-blur`: full strength up to `1 - blur` of
/// the way out, then a smoothstep down to nothing at the edge. A blur above
/// 1 starts the fade before the centre, so even the centre is faded. Fill
/// and ring each fade their own colour, where MapLibre mixes the two first
/// and fades the mix; the two differ only where a translucent ring overlaps
/// the fill inside the fade.
fn set_paint_color(
    paint: &mut Paint<'_>,
    color: tiny_skia::Color,
    blur: f32,
    centre: (f32, f32),
    outer: f32,
) {
    if blur <= 0.0 || outer <= 0.0 {
        paint.set_color(color);
        return;
    }
    let from = (1.0 - blur).max(0.0);
    let stops = (0..BLUR_STOPS)
        .map(|i| {
            let d = from + (1.0 - from) * i as f32 / (BLUR_STOPS - 1) as f32;
            // How far inside the edge `d` is, as a fraction of the fade.
            let u = ((1.0 - d) / blur).clamp(0.0, 1.0);
            let mut c = color;
            c.set_alpha(color.alpha() * u * u * (3.0 - 2.0 * u));
            GradientStop::new(d, c)
        })
        .collect();
    let centre = tiny_skia::Point::from_xy(centre.0, centre.1);
    match RadialGradient::new(
        centre,
        0.0,
        centre,
        outer,
        stops,
        SpreadMode::Pad,
        Transform::identity(),
    ) {
        Some(shader) => paint.shader = shader,
        None => paint.set_color(color),
    }
}

struct CirclesNode {
    radius: In<f64>,
    color: In<[f32; 4]>,
    opacity: In<f64>,
    stroke_width: In<f64>,
    stroke_color: In<[f32; 4]>,
    /// The ring's own opacity (MapLibre `circle-stroke-opacity`). Absent,
    /// the ring takes `opacity` as the fill does.
    stroke_opacity: Option<In<f64>>,
    /// Where the ring sits against `radius`.
    stroke_align: InEnum<StrokeAlign>,
    /// MapLibre `circle-blur`: the fraction of the circle's outer radius over
    /// which it fades out to its edge. 0 keeps a crisp edge.
    blur: In<f64>,
    radius_expr: Option<maplibre_expr::Expr>,
    color_expr: Option<maplibre_expr::Expr>,
    opacity_expr: Option<maplibre_expr::Expr>,
    stroke_width_expr: Option<maplibre_expr::Expr>,
    stroke_color_expr: Option<maplibre_expr::Expr>,
    stroke_opacity_expr: Option<maplibre_expr::Expr>,
    blur_expr: Option<maplibre_expr::Expr>,
    radius_expr_src: Option<String>,
    color_expr_src: Option<String>,
    opacity_expr_src: Option<String>,
    stroke_width_expr_src: Option<String>,
    stroke_color_expr_src: Option<String>,
    stroke_opacity_expr_src: Option<String>,
    blur_expr_src: Option<String>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for CirclesNode {
    fn op_name(&self) -> &'static str {
        "circles"
    }

    /// Nothing upstream of this op may be dropped.
    ///
    /// `radius` has no declared ceiling, and an expression-driven one
    /// has no static value at all.
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
        let feats = downcast_features(
            inputs[0]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("features".into()))?,
        )?;
        if !feats.has_points() {
            return Ok(empty_raster(ctx));
        }

        // Constants, resolved once. Data-driven exprs (if present) override
        // these per feature group; whichever expr is absent uses the constant.
        let const_radius = (self.radius.get(ctx, inputs)? as f32).max(0.0);
        let const_color = color_f32_to_u8(self.color.get(ctx, inputs)?);
        let const_opacity = self.opacity.get(ctx, inputs)? as f32;
        let const_stroke_width = (self.stroke_width.get(ctx, inputs)? as f32).max(0.0);
        let const_stroke_color = color_f32_to_u8(self.stroke_color.get(ctx, inputs)?);
        let const_stroke_opacity = match &self.stroke_opacity {
            Some(o) => Some(o.get(ctx, inputs)? as f32),
            None => None,
        };
        let const_blur = (self.blur.get(ctx, inputs)? as f32).max(0.0);
        let stroke_align = self.stroke_align.get(ctx)?;

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
            // Without a `stroke-opacity` of its own the ring takes `opacity`.
            let stroke_opacity = match (&self.stroke_opacity_expr, const_stroke_opacity) {
                (None, None) => opacity,
                (_, fallback) => eval_number(
                    &self.stroke_opacity_expr,
                    &ectx,
                    fallback.unwrap_or(opacity),
                ),
            };
            let stroke_color = tint_alpha_color(stroke_rgba, stroke_opacity);
            let blur = eval_number(&self.blur_expr, &ectx, const_blur).max(0.0);
            // The circle ends at the ring's outer edge, wherever
            // `stroke-align` puts the ring; that edge is what the blur fades
            // to.
            let (ring_radius, ring_width, outer) = if stroke_width > 0.0 {
                stroke_align.ring(radius, stroke_width)
            } else {
                (radius, 0.0, radius)
            };

            let pm = canvas.pixmap_mut();
            for &(x, y) in &group.points {
                let px = x as f32 * sx + pad;
                let py = y as f32 * sy + pad;
                let mut pb = PathBuilder::new();
                pb.push_circle(px, py, radius);
                let Some(path) = pb.finish() else {
                    continue;
                };
                // A centred ring runs along the fill's own outline.
                let ring_path;
                let ring = if ring_radius == radius {
                    Some(&path)
                } else {
                    let mut pb = PathBuilder::new();
                    pb.push_circle(px, py, ring_radius);
                    ring_path = pb.finish();
                    ring_path.as_ref()
                };
                // Fill first, stroke outline on top (matches MapLibre
                // `circle-stroke` sitting above the fill).
                let mut fill_paint = Paint::default();
                set_paint_color(&mut fill_paint, fill_color, blur, (px, py), outer);
                fill_paint.anti_alias = true;
                pm.fill_path(
                    &path,
                    &fill_paint,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
                if let (true, Some(ring)) = (ring_width > 0.0, ring) {
                    let mut sp = Paint::default();
                    set_paint_color(&mut sp, stroke_color, blur, (px, py), outer);
                    sp.anti_alias = true;
                    pm.stroke_path(
                        ring,
                        &sp,
                        &Stroke {
                            width: ring_width,
                            ..Default::default()
                        },
                        Transform::identity(),
                        None,
                    );
                }
            }
        };

        for group in &feats.groups {
            paint_group(&mut canvas, group);
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
        if let Some(o) = &self.stroke_opacity {
            h.update(b"strokeopacity");
            o.param_hash(h);
        }
        self.blur.param_hash(h);
        self.stroke_align.param_hash(h);
        for (tag, src) in [
            (b"radiusexpr".as_slice(), &self.radius_expr_src),
            (b"colorexpr".as_slice(), &self.color_expr_src),
            (b"opacityexpr".as_slice(), &self.opacity_expr_src),
            (b"strokewidthexpr".as_slice(), &self.stroke_width_expr_src),
            (b"strokecolorexpr".as_slice(), &self.stroke_color_expr_src),
            (
                b"strokeopacityexpr".as_slice(),
                &self.stroke_opacity_expr_src,
            ),
            (b"blurexpr".as_slice(), &self.blur_expr_src),
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
        let stroke_opacity = if fields.contains_key("stroke-opacity") {
            Some(r.number("stroke-opacity")?)
        } else {
            None
        };
        let blur = r.number_or("blur", 0.0)?;
        let stroke_align = r.enum_or("stroke-align", StrokeAlign::Center, STROKE_ALIGNS)?;
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
        let (stroke_opacity_expr, stroke_opacity_expr_src) =
            parse_expr_field(fields, "stroke-opacity-expr", &maplibre_expr::Type::Number)?;
        let (blur_expr, blur_expr_src) =
            parse_expr_field(fields, "blur-expr", &maplibre_expr::Type::Number)?;

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
                stroke_opacity,
                stroke_align,
                blur,
                radius_expr,
                color_expr,
                opacity_expr,
                stroke_width_expr,
                stroke_color_expr,
                stroke_opacity_expr,
                blur_expr,
                radius_expr_src,
                color_expr_src,
                opacity_expr_src,
                stroke_width_expr_src,
                stroke_color_expr_src,
                stroke_opacity_expr_src,
                blur_expr_src,
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
                    "description": "A MapLibre number expression giving opacity, evaluated per feature group; multiplies the fill's alpha, and the stroke ring's too unless `stroke-opacity` or `stroke-opacity-expr` is set. Overrides the constant `opacity`.",
                },
                "blur": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                          "description": "MapLibre `circle-blur`: the fraction of the circle's outer radius (to the outside of the stroke ring) over which fill and ring fade out to the edge, with a smoothstep. 0 (the default) keeps a crisp edge; 1 fades from the centre; above 1 fades even the centre." })),
                "blur-expr": {
                    "description": "A MapLibre number expression for `blur`, evaluated per feature group; overrides the constant `blur`.",
                },
                "stroke-width": schema_frag::px_number(),
                "stroke-width-expr": {
                    "description": "A MapLibre number expression (px) for the stroke ring, evaluated per feature group; overrides the constant `stroke-width`. A ring is drawn only when width > 0.",
                },
                "stroke-align": schema_frag::in_enum(serde_json::json!({ "type": "string", "enum": enum_names(STROKE_ALIGNS), "default": "center",
                                  "description": "Where the stroke ring sits against `radius`. `center` straddles it, half inside and half outside; `outside` lies wholly outside it, from `radius` to `radius + stroke-width`, as MapLibre draws `circle-stroke-width`; `inside` lies wholly within it, so the circle never grows past `radius`. The fill always reaches `radius`." })),
                "stroke-color": schema_frag::color(),
                "stroke-color-expr": {
                    "description": "A MapLibre color expression for the stroke ring, evaluated per feature group; overrides the constant `stroke-color`.",
                },
                "stroke-opacity": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "maximum": 1.0,
                                    "description": "The stroke ring's opacity (MapLibre `circle-stroke-opacity`), in place of `opacity`, which then applies to the fill alone. Left out, the ring takes `opacity`." })),
                "stroke-opacity-expr": {
                    "description": "A MapLibre number expression for the stroke ring's opacity, evaluated per feature group; overrides `stroke-opacity`. A group whose expression doesn't resolve to a number falls back to `stroke-opacity`, or to `opacity` when that is left out.",
                },
            },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(CirclesFactory);
