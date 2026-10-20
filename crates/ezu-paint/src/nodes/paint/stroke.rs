//! `stroke` — `Features -> Raster`. A crisp, constant-width `tiny-skia`
//! vector stroke along polylines, with cap/join and optional dashing. This
//! is the sharp counterpart to `line` (a painterly hokusai brush) — use it
//! to reproduce clean cartographic road/boundary lines (e.g. MapLibre).

use std::sync::Arc;

use ezu_graph::{
    enum_names, schema_frag, take_input_ref, BuiltNode, Connection, EnumTable, EvalCtx, EvalError,
    FactoryCtx, FactoryError, In, InEnum, InReader, InfluenceCtx, Node, NodeFactory, PortKind,
    PortSpec, PortValue,
};
use serde_json::Value;
use tiny_skia::{LineCap, LineJoin};
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    canvas_into_raster, color_f32_to_u8, downcast_features, empty_raster, make_canvas,
    tint_alpha_color,
};
use crate::{paint_strokes, StrokeStyle};

pub(super) const CAPS: EnumTable<LineCap> = &[
    ("butt", LineCap::Butt),
    ("round", LineCap::Round),
    ("square", LineCap::Square),
];

pub(super) const JOINS: EnumTable<LineJoin> = &[
    ("miter", LineJoin::Miter),
    ("round", LineJoin::Round),
    ("bevel", LineJoin::Bevel),
];

/// What `dasharray`'s lengths are measured in.
#[derive(Debug, Clone, Copy, PartialEq)]
enum DashUnit {
    /// Pixels, whatever the stroke's width.
    Px,
    /// Multiples of the stroke's width, as MapLibre's `line-dasharray` is:
    /// each feature group's dashes scale with the width that group resolves
    /// to, `width-expr` included.
    Width,
}

const DASH_UNITS: EnumTable<DashUnit> = &[("px", DashUnit::Px), ("width", DashUnit::Width)];

struct StrokeNode {
    color: In<[f32; 4]>,
    width_px: In<f64>,
    gap_width_px: In<f64>,
    opacity: In<f64>,
    cap: InEnum<LineCap>,
    join: InEnum<LineJoin>,
    /// On/off dash pattern (`None` = solid), in `dash_unit`s. Each length
    /// may be a `$param`, so the pattern resolves once per eval.
    dash: Option<Vec<In<f64>>>,
    dash_unit: InEnum<DashUnit>,
    /// Optional data-driven stroke color: a MapLibre color expression
    /// evaluated per feature group. When set, it overrides the constant
    /// `color` for groups whose expression resolves to a color.
    color_expr: Option<maplibre_expr::Expr>,
    /// Optional data-driven stroke width (px): a MapLibre number expression
    /// evaluated per feature group. When set, it overrides `width-px`.
    width_expr: Option<maplibre_expr::Expr>,
    /// Optional data-driven casing gap (px): a MapLibre number expression
    /// evaluated per feature group. When set, it overrides `gap-width-px`.
    gap_width_expr: Option<maplibre_expr::Expr>,
    /// Optional data-driven opacity: a MapLibre number expression evaluated
    /// per feature group. When set, it overrides the constant `opacity`.
    opacity_expr: Option<maplibre_expr::Expr>,
    /// Raw `color-expr` / `width-expr` / `gap-width-expr` / `opacity-expr`
    /// JSON text, for a stable hash.
    color_expr_src: Option<String>,
    width_expr_src: Option<String>,
    gap_width_expr_src: Option<String>,
    opacity_expr_src: Option<String>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for StrokeNode {
    fn op_name(&self) -> &'static str {
        "stroke"
    }

    /// A stroke marks the canvas half a width either side of the path,
    /// a casing gap pushing that out further, and up to the miter limit
    /// of it at a sharp corner. An expression-driven width has no
    /// static value at all, so it cannot be bounded.
    fn influence_pad(&self, ctx: &InfluenceCtx<'_>) -> u32 {
        if self.width_expr.is_some() || self.gap_width_expr.is_some() {
            return InfluenceCtx::UNBOUNDED;
        }
        let (Some(w), Some(gap)) = (
            self.width_px.static_bound(),
            self.gap_width_px.static_bound(),
        ) else {
            return InfluenceCtx::UNBOUNDED;
        };
        // The casing footprint, times tiny-skia's default miter limit
        // of 4 over the naive half-width.
        ctx.plus(2.0 * (gap.abs() + 2.0 * w.abs()))
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
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
        if !feats.has_lines() {
            return Ok(empty_raster(ctx));
        }
        let rgba8 = color_f32_to_u8(self.color.get(ctx, inputs)?);
        let opacity = self.opacity.get(ctx, inputs)? as f32;
        let const_color = tint_alpha_color(rgba8, opacity);
        let const_width = (self.width_px.get(ctx, inputs)? as f32).max(0.0);
        let const_gap = (self.gap_width_px.get(ctx, inputs)? as f32).max(0.0);
        let cap = self.cap.get(ctx)?;
        let join = self.join.get(ctx)?;
        // Resolved once here; every group's `StrokeStyle` clones it.
        let dash = match &self.dash {
            None => None,
            Some(d) => Some(
                d.iter()
                    .map(|v| v.get(ctx, inputs).map(|n| n as f32))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        };
        let dash_unit = self.dash_unit.get(ctx)?;
        // The pattern in pixels for a stroke of `width`.
        let dash_px = |width: f32| match (&dash, dash_unit) {
            (Some(d), DashUnit::Width) => Some(d.iter().map(|l| l * width).collect()),
            (d, _) => d.clone(),
        };
        let mut canvas = make_canvas(ctx)?;

        if self.color_expr.is_some()
            || self.width_expr.is_some()
            || self.gap_width_expr.is_some()
            || self.opacity_expr.is_some()
        {
            // Data-driven stroke: resolve color, width, and/or opacity per
            // feature group and accumulate each group's lines onto the same
            // canvas. Whichever expression is absent (or errors for a group)
            // falls back to the constant `color` / `width-px` / `opacity`.
            let z = ctx.tile.z;
            for group in &feats.groups {
                let ectx = crate::render::group_expr_context(group, z);
                let alpha = match &self.opacity_expr {
                    Some(expr) => match maplibre_expr::evaluate(expr, &ectx) {
                        Ok(maplibre_expr::Value::Number(n)) => n as f32,
                        _ => opacity,
                    },
                    None => opacity,
                };
                let color = match &self.color_expr {
                    Some(expr) => match maplibre_expr::evaluate(expr, &ectx) {
                        Ok(maplibre_expr::Value::Color(c)) => tint_alpha_color(
                            // maplibre-expr `Color` stores straight channels in
                            // `0..=1`, exactly like a parsed hex literal — so an
                            // opaque data-driven color paints the same pixels as
                            // the constant `color` path.
                            color_f32_to_u8([c.r as f32, c.g as f32, c.b as f32, c.a as f32]),
                            alpha,
                        ),
                        _ => tint_alpha_color(rgba8, alpha),
                    },
                    None => tint_alpha_color(rgba8, alpha),
                };
                let width = match &self.width_expr {
                    Some(expr) => match maplibre_expr::evaluate(expr, &ectx) {
                        Ok(maplibre_expr::Value::Number(n)) => (n as f32).max(0.0),
                        _ => const_width,
                    },
                    None => const_width,
                };
                let gap = match &self.gap_width_expr {
                    Some(expr) => match maplibre_expr::evaluate(expr, &ectx) {
                        Ok(maplibre_expr::Value::Number(n)) => (n as f32).max(0.0),
                        _ => const_gap,
                    },
                    None => const_gap,
                };
                let style = StrokeStyle {
                    color,
                    width,
                    cap,
                    join,
                    dash: dash_px(width),
                    gap,
                };
                paint_strokes(&mut canvas, &group.lines, feats.extent, &style);
            }
            return Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))));
        }

        let style = StrokeStyle {
            color: const_color,
            width: const_width,
            cap,
            join,
            dash: dash_px(const_width),
            gap: const_gap,
        };
        let lines: Vec<_> = feats.lines().cloned().collect();
        paint_strokes(&mut canvas, &lines, feats.extent, &style);
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"stroke");
        self.color.param_hash(h);
        self.width_px.param_hash(h);
        self.gap_width_px.param_hash(h);
        self.opacity.param_hash(h);
        self.cap.param_hash(h);
        self.join.param_hash(h);
        if let Some(d) = &self.dash {
            h.update(&[1]);
            for v in d {
                v.param_hash(h);
            }
        } else {
            h.update(&[0]);
        }
        self.dash_unit.param_hash(h);
        if let Some(s) = &self.color_expr_src {
            h.update(b"colorexpr");
            h.update(s.as_bytes());
        }
        if let Some(s) = &self.width_expr_src {
            h.update(b"widthexpr");
            h.update(s.as_bytes());
        }
        if let Some(s) = &self.gap_width_expr_src {
            h.update(b"gapwidthexpr");
            h.update(s.as_bytes());
        }
        if let Some(s) = &self.opacity_expr_src {
            h.update(b"opacityexpr");
            h.update(s.as_bytes());
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct StrokeFactory;
impl NodeFactory for StrokeFactory {
    fn op_name(&self) -> &'static str {
        "stroke"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let mut r = InReader::new(fields, ctx, 1);
        let cap = r.enum_or("cap", LineCap::Butt, CAPS)?;
        let join = r.enum_or("join", LineJoin::Miter, JOINS)?;
        let dash = match fields.get("dasharray") {
            None | Some(Value::Null) => None,
            Some(v) => {
                let arr = v.as_array().ok_or_else(|| FactoryError::BadField {
                    field: "dasharray".into(),
                    msg: "expected an array of numbers (pixels)".into(),
                })?;
                let mut pat = Vec::with_capacity(arr.len());
                for (i, x) in arr.iter().enumerate() {
                    pat.push(r.nested(&format!("dasharray[{i}]"), x)?);
                }
                if pat.is_empty() {
                    None
                } else {
                    Some(pat)
                }
            }
        };
        let color = r.color("color")?;
        let width_px = r.number_or("width-px", 1.0)?;
        let gap_width_px = r.number_or("gap-width-px", 0.0)?;
        let opacity = r.number_or("opacity", 1.0)?;
        let dash_unit = r.enum_or("dash-unit", DashUnit::Px, DASH_UNITS)?;
        let parts = r.finish();

        // `color-expr`: a raw MapLibre color expression, compiled once and
        // evaluated per feature group at paint time. Overrides `color`.
        let (color_expr, color_expr_src) = match fields.get("color-expr") {
            Some(v) => {
                let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
                    field: "color-expr".into(),
                    msg: e.to_string(),
                })?;
                // Type-check against `Color` so string branches coerce to
                // color values, matching how MapLibre resolves a color-typed
                // paint property.
                let expr =
                    maplibre_expr::typecheck(&expr, Some(&maplibre_expr::Type::Color), false)
                        .map_err(|e| FactoryError::BadField {
                            field: "color-expr".into(),
                            msg: e.to_string(),
                        })?;
                (Some(expr), Some(v.to_string()))
            }
            None => (None, None),
        };
        // `width-expr`: a raw MapLibre number expression (px), evaluated per
        // feature group at paint time. Overrides `width-px`.
        let (width_expr, width_expr_src) = match fields.get("width-expr") {
            Some(v) => {
                let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
                    field: "width-expr".into(),
                    msg: e.to_string(),
                })?;
                let expr =
                    maplibre_expr::typecheck(&expr, Some(&maplibre_expr::Type::Number), false)
                        .map_err(|e| FactoryError::BadField {
                            field: "width-expr".into(),
                            msg: e.to_string(),
                        })?;
                (Some(expr), Some(v.to_string()))
            }
            None => (None, None),
        };
        // `gap-width-expr`: a raw MapLibre number expression (px), evaluated
        // per feature group at paint time. Overrides `gap-width-px`.
        let (gap_width_expr, gap_width_expr_src) = match fields.get("gap-width-expr") {
            Some(v) => {
                let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
                    field: "gap-width-expr".into(),
                    msg: e.to_string(),
                })?;
                let expr =
                    maplibre_expr::typecheck(&expr, Some(&maplibre_expr::Type::Number), false)
                        .map_err(|e| FactoryError::BadField {
                            field: "gap-width-expr".into(),
                            msg: e.to_string(),
                        })?;
                (Some(expr), Some(v.to_string()))
            }
            None => (None, None),
        };
        // `opacity-expr`: a raw MapLibre number expression, evaluated per
        // feature group at paint time. Overrides `opacity`.
        let (opacity_expr, opacity_expr_src) = match fields.get("opacity-expr") {
            Some(v) => {
                let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
                    field: "opacity-expr".into(),
                    msg: e.to_string(),
                })?;
                let expr =
                    maplibre_expr::typecheck(&expr, Some(&maplibre_expr::Type::Number), false)
                        .map_err(|e| FactoryError::BadField {
                            field: "opacity-expr".into(),
                            msg: e.to_string(),
                        })?;
                (Some(expr), Some(v.to_string()))
            }
            None => (None, None),
        };

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
            node: Box::new(StrokeNode {
                color,
                width_px,
                gap_width_px,
                opacity,
                cap,
                join,
                dash,
                dash_unit,
                color_expr,
                width_expr,
                gap_width_expr,
                opacity_expr,
                color_expr_src,
                width_expr_src,
                gap_width_expr_src,
                opacity_expr_src,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Crisp constant-width vector stroke along feature polylines (tiny-skia), with cap/join and an optional `dasharray`, in pixels or in stroke widths. The sharp counterpart to `line` (painterly brush) — for clean cartographic road/boundary lines.",
            "properties": {
                "features": schema_frag::node_ref(),
                "color": schema_frag::color(),
                "color-expr": {
                    "description": "A MapLibre color expression (JSON array, e.g. [\"match\", [\"get\", \"class\"], \"river\", \"#48b\", \"#888\"]), evaluated per feature group at paint time. When present it overrides the constant `color`; a group whose expression doesn't resolve to a color falls back to `color`. Unlocks data-driven stroke colors.",
                },
                "width-px": schema_frag::px_number(),
                "width-expr": {
                    "description": "A MapLibre number expression (JSON array, e.g. [\"interpolate\", [\"linear\"], [\"zoom\"], 10, 1, 16, 4]) giving stroke width in pixels, evaluated per feature group at paint time. When present it overrides the constant `width-px`; a group whose expression doesn't resolve to a number falls back to `width-px`.",
                },
                "gap-width-px": schema_frag::in_number(serde_json::json!({
                    "type": "number",
                    "default": 0,
                    "description": "MapLibre `line-gap-width`: when > 0 the stroke becomes a casing — two parallel bands of `width-px` each, their inner edges this many pixels apart (outer footprint `gap + 2 * width`, centred on the line). `0` strokes the centreline.",
                })),
                "gap-width-expr": {
                    "description": "A MapLibre number expression (JSON array) giving the casing gap in pixels, evaluated per feature group at paint time. When present it overrides the constant `gap-width-px`; a group whose expression doesn't resolve to a number falls back to `gap-width-px`.",
                },
                "opacity": schema_frag::unit_number(),
                "opacity-expr": {
                    "description": "A MapLibre number expression (JSON array) giving stroke opacity, evaluated per feature group at paint time. When present it overrides the constant `opacity`; a group whose expression doesn't resolve to a number falls back to `opacity`.",
                },
                "cap": schema_frag::in_enum(serde_json::json!({ "type": "string", "enum": enum_names(CAPS), "default": "butt" })),
                "join": schema_frag::in_enum(serde_json::json!({ "type": "string", "enum": enum_names(JOINS), "default": "miter" })),
                "dasharray": { "type": "array",
                               "items": schema_frag::nested_number(serde_json::json!({ "type": "number", "minimum": 0.0 })),
                               "description": "On/off lengths, in pixels or, with `dash-unit: width`, in multiples of the stroke width; omit for solid. Each length may be a `$param`." },
                "dash-unit": schema_frag::in_enum(serde_json::json!({ "type": "string", "enum": enum_names(DASH_UNITS), "default": "px",
                               "description": "What `dasharray` is measured in. `px` (the default) keeps the dashes the same length whatever the width; `width` multiplies each length by the stroke's width, as MapLibre's `line-dasharray` does, so with a `width-expr` each feature group's dashes scale with the width it resolves to. A casing's dashes scale with `width-px`, not with the gap." })),
            },
            "required": ["features", "color"],
        })
    }
}

ezu_graph::submit_node!(StrokeFactory);
