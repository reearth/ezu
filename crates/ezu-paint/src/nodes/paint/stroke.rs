//! `stroke` — `Features -> Raster`. A crisp, constant-width `tiny-skia`
//! vector stroke along polylines, with cap/join and optional dashing. This
//! is the sharp counterpart to `line` (a painterly hokusai brush) — use it
//! to reproduce clean cartographic road/boundary lines (e.g. MapLibre).

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use tiny_skia::{LineCap, LineJoin};
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    canvas_into_raster, color_f32_to_u8, downcast_features, empty_raster, make_canvas,
    read_string_or, tint_alpha_color,
};
use crate::{paint_strokes, StrokeStyle};

struct StrokeNode {
    color: In<[f32; 4]>,
    width_px: In<f64>,
    opacity: In<f64>,
    cap: LineCap,
    join: LineJoin,
    /// On/off dash pattern in pixels (`None` = solid).
    dash: Option<Vec<f32>>,
    /// Optional data-driven stroke color: a MapLibre color expression
    /// evaluated per feature group. When set, it overrides the constant
    /// `color` for groups whose expression resolves to a color.
    color_expr: Option<maplibre_expr::Expr>,
    /// Optional data-driven stroke width (px): a MapLibre number expression
    /// evaluated per feature group. When set, it overrides `width-px`.
    width_expr: Option<maplibre_expr::Expr>,
    /// Optional data-driven opacity: a MapLibre number expression evaluated
    /// per feature group. When set, it overrides the constant `opacity`.
    opacity_expr: Option<maplibre_expr::Expr>,
    /// Raw `color-expr` / `width-expr` / `opacity-expr` JSON text, for a
    /// stable hash.
    color_expr_src: Option<String>,
    width_expr_src: Option<String>,
    opacity_expr_src: Option<String>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for StrokeNode {
    fn op_name(&self) -> &'static str {
        "stroke"
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
        if feats.lines.is_empty() {
            return Ok(empty_raster(ctx));
        }
        let rgba8 = color_f32_to_u8(self.color.get(ctx, inputs)?);
        let opacity = self.opacity.get(ctx, inputs)? as f32;
        let const_color = tint_alpha_color(rgba8, opacity);
        let const_width = (self.width_px.get(ctx, inputs)? as f32).max(0.0);
        let mut canvas = make_canvas(ctx)?;

        if self.color_expr.is_some() || self.width_expr.is_some() || self.opacity_expr.is_some() {
            // Data-driven stroke: resolve color, width, and/or opacity per
            // feature group and accumulate each group's lines onto the same
            // canvas. Whichever expression is absent (or errors for a group)
            // falls back to the constant `color` / `width-px` / `opacity`.
            //
            // Synthetic geometry (e.g. `literal-geometry`) carries no groups;
            // fall back to a single empty-property group over the flat lines.
            let z = ctx.tile.z;
            let paint_group = |canvas: &mut _, group: &crate::nodes::common::FeatureGroup| {
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
                let style = StrokeStyle {
                    color,
                    width,
                    cap: self.cap,
                    join: self.join,
                    dash: self.dash.clone(),
                };
                paint_strokes(canvas, &group.lines, feats.extent, &style);
            };
            if feats.groups.is_empty() {
                let synthetic = crate::nodes::common::FeatureGroup {
                    properties: std::collections::HashMap::new(),
                    polygons: Vec::new(),
                    lines: feats.lines.clone(),
                    points: Vec::new(),
                };
                paint_group(&mut canvas, &synthetic);
            } else {
                for group in &feats.groups {
                    paint_group(&mut canvas, group);
                }
            }
            return Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))));
        }

        let style = StrokeStyle {
            color: const_color,
            width: const_width,
            cap: self.cap,
            join: self.join,
            dash: self.dash.clone(),
        };
        paint_strokes(&mut canvas, &feats.lines, feats.extent, &style);
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"stroke");
        self.color.param_hash(h);
        self.width_px.param_hash(h);
        self.opacity.param_hash(h);
        h.update(&[cap_tag(self.cap), join_tag(self.join)]);
        if let Some(d) = &self.dash {
            h.update(&[1]);
            for v in d {
                h.update(&v.to_le_bytes());
            }
        } else {
            h.update(&[0]);
        }
        if let Some(s) = &self.color_expr_src {
            h.update(b"colorexpr");
            h.update(s.as_bytes());
        }
        if let Some(s) = &self.width_expr_src {
            h.update(b"widthexpr");
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

fn cap_tag(c: LineCap) -> u8 {
    match c {
        LineCap::Butt => 0,
        LineCap::Round => 1,
        LineCap::Square => 2,
    }
}

fn join_tag(j: LineJoin) -> u8 {
    match j {
        LineJoin::Miter => 0,
        LineJoin::Round => 1,
        LineJoin::Bevel => 2,
        LineJoin::MiterClip => 3,
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
        let cap = match read_string_or(fields, "cap", ctx, "butt")?.as_str() {
            "butt" => LineCap::Butt,
            "round" => LineCap::Round,
            "square" => LineCap::Square,
            other => {
                return Err(FactoryError::BadField {
                    field: "cap".into(),
                    msg: format!("expected butt/round/square, got `{other}`"),
                })
            }
        };
        let join = match read_string_or(fields, "join", ctx, "miter")?.as_str() {
            "miter" => LineJoin::Miter,
            "round" => LineJoin::Round,
            "bevel" => LineJoin::Bevel,
            other => {
                return Err(FactoryError::BadField {
                    field: "join".into(),
                    msg: format!("expected miter/round/bevel, got `{other}`"),
                })
            }
        };
        let dash = match fields.get("dasharray") {
            None | Some(Value::Null) => None,
            Some(v) => {
                let arr = v.as_array().ok_or_else(|| FactoryError::BadField {
                    field: "dasharray".into(),
                    msg: "expected an array of numbers (pixels)".into(),
                })?;
                let pat: Vec<f32> = arr
                    .iter()
                    .filter_map(|x| x.as_f64())
                    .map(|x| x as f32)
                    .collect();
                if pat.is_empty() {
                    None
                } else {
                    Some(pat)
                }
            }
        };

        let mut r = InReader::new(fields, ctx, 1);
        let color = r.color("color")?;
        let width_px = r.number_or("width-px", 1.0)?;
        let opacity = r.number_or("opacity", 1.0)?;
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
                opacity,
                cap,
                join,
                dash,
                color_expr,
                width_expr,
                opacity_expr,
                color_expr_src,
                width_expr_src,
                opacity_expr_src,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Crisp constant-width vector stroke along feature polylines (tiny-skia), with cap/join and optional pixel `dasharray`. The sharp counterpart to `line` (painterly brush) — for clean cartographic road/boundary lines.",
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
                "opacity": schema_frag::unit_number(),
                "opacity-expr": {
                    "description": "A MapLibre number expression (JSON array) giving stroke opacity, evaluated per feature group at paint time. When present it overrides the constant `opacity`; a group whose expression doesn't resolve to a number falls back to `opacity`.",
                },
                "cap": { "type": "string", "enum": ["butt", "round", "square"], "default": "butt" },
                "join": { "type": "string", "enum": ["miter", "round", "bevel"], "default": "miter" },
                "dasharray": { "type": "array", "items": { "type": "number" }, "description": "On/off lengths in pixels; omit for solid." },
            },
            "required": ["features", "color"],
        })
    }
}

ezu_graph::submit_node!(StrokeFactory);
