//! `fill-solid` — `Features -> Raster`. Wraps
//! [`paint_polygons`](crate::paint_polygons): solid fill, optional
//! outline, optional Gaussian blur.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    canvas_into_raster, color_f32_to_u8, downcast_features, empty_raster, make_canvas,
    rgba8_to_color, tint_alpha_color,
};
use crate::{paint_polygons, WatercolorStyle};

struct FillSolidNode {
    fill: In<[f32; 4]>,
    fill_alpha: In<f64>,
    edge: Option<In<[f32; 4]>>,
    edge_width: In<f64>,
    blur_sigma: In<f64>,
    /// Build-time upper bound on `blur-sigma`, for pad propagation.
    blur_sigma_bound: f32,
    /// Optional data-driven fill: a MapLibre color expression evaluated per
    /// feature group. When set, it overrides the constant `fill` (each group
    /// paints in its own resolved color).
    fill_expr: Option<maplibre_expr::Expr>,
    /// Optional data-driven opacity: a MapLibre number expression evaluated per
    /// feature group. When set, it overrides the constant `fill-alpha`.
    opacity_expr: Option<maplibre_expr::Expr>,
    /// Raw `fill-expr` / `opacity-expr` JSON text, kept only for a stable hash.
    fill_expr_src: Option<String>,
    opacity_expr_src: Option<String>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for FillSolidNode {
    fn op_name(&self) -> &'static str {
        "fill-solid"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + (3.0 * self.blur_sigma_bound.max(0.0)).ceil() as u32
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
        let fill_alpha = self.fill_alpha.get(ctx, inputs)? as f32;
        let edge = match &self.edge {
            Some(e) => Some(color_f32_to_u8(e.get(ctx, inputs)?)),
            None => None,
        };
        let edge_color = edge.map(rgba8_to_color);
        let edge_width = self.edge_width.get(ctx, inputs)? as f32;
        let blur_sigma = self.blur_sigma.get(ctx, inputs)? as f32;
        let mut canvas = make_canvas(ctx)?;

        if self.fill_expr.is_some() || self.opacity_expr.is_some() {
            // Data-driven fill: resolve a color and/or opacity per feature
            // group and accumulate each group's polygons onto the same
            // canvas. Whichever expression is absent (or errors for a group)
            // falls back to the constant `fill` / `fill-alpha`.
            let const_fill = color_f32_to_u8(self.fill.get(ctx, inputs)?);
            let z = ctx.tile.z;
            for group in &feats.groups {
                let ectx = crate::render::group_expr_context(group, z);
                // maplibre-expr `Color` stores straight (non-premultiplied)
                // channels in `0..=1`, exactly like a parsed `#rrggbb[aa]`
                // literal — so an opaque data-driven color paints the same
                // pixels as the constant `fill` path.
                let fill = match &self.fill_expr {
                    Some(expr) => match maplibre_expr::evaluate(expr, &ectx) {
                        Ok(maplibre_expr::Value::Color(c)) => {
                            color_f32_to_u8([c.r as f32, c.g as f32, c.b as f32, c.a as f32])
                        }
                        _ => const_fill,
                    },
                    None => const_fill,
                };
                let alpha = match &self.opacity_expr {
                    Some(expr) => match maplibre_expr::evaluate(expr, &ectx) {
                        Ok(maplibre_expr::Value::Number(n)) => n as f32,
                        _ => fill_alpha,
                    },
                    None => fill_alpha,
                };
                let style = WatercolorStyle {
                    fill: tint_alpha_color(fill, alpha),
                    edge: edge_color,
                    edge_width,
                    blur_sigma,
                };
                paint_polygons(&mut canvas, &group.polygons, feats.extent, &style);
            }
            return Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))));
        }

        let fill = color_f32_to_u8(self.fill.get(ctx, inputs)?);
        let style = WatercolorStyle {
            fill: tint_alpha_color(fill, fill_alpha),
            edge: edge_color,
            edge_width,
            blur_sigma,
        };
        let polygons: Vec<_> = feats.polygons().cloned().collect();
        paint_polygons(&mut canvas, &polygons, feats.extent, &style);
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"fill-solid");
        self.fill.param_hash(h);
        self.fill_alpha.param_hash(h);
        if let Some(e) = &self.edge {
            h.update(&[1]);
            e.param_hash(h);
        } else {
            h.update(&[0]);
        }
        self.edge_width.param_hash(h);
        self.blur_sigma.param_hash(h);
        if let Some(s) = &self.fill_expr_src {
            h.update(b"fillexpr");
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

pub(super) struct FillSolidFactory;
impl NodeFactory for FillSolidFactory {
    fn op_name(&self) -> &'static str {
        "fill-solid"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let mut r = InReader::new(fields, ctx, 1);
        let fill = r.color("fill")?;
        let fill_alpha = r.number_or("fill-alpha", 1.0)?;
        let edge = r.color_opt("edge")?;
        let edge_width = r.number_or("edge-width", 1.0)?;
        let blur_sigma = r.number_or("blur-sigma", 0.0)?;
        // `fill-expr`: a raw MapLibre color expression, compiled once and
        // evaluated per feature group at paint time. Overrides `fill`.
        let (fill_expr, fill_expr_src) = match fields.get("fill-expr") {
            Some(v) => {
                let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
                    field: "fill-expr".into(),
                    msg: e.to_string(),
                })?;
                // Type-check against `Color` so string branches (e.g. a
                // `["match", …, "#ff0000"]`) coerce to color values, matching
                // how MapLibre resolves a color-typed paint property.
                let expr =
                    maplibre_expr::typecheck(&expr, Some(&maplibre_expr::Type::Color), false)
                        .map_err(|e| FactoryError::BadField {
                            field: "fill-expr".into(),
                            msg: e.to_string(),
                        })?;
                (Some(expr), Some(v.to_string()))
            }
            None => (None, None),
        };
        // `opacity-expr`: a raw MapLibre number expression, evaluated per
        // feature group at paint time. Overrides `fill-alpha`.
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
        let parts = r.finish();
        let blur_sigma_bound = blur_sigma
            .static_bound()
            .ok_or_else(|| FactoryError::BadField {
                field: "blur-sigma".into(),
                msg: "pad depends on blur-sigma at build time: use a literal, or a `$param` \
                          with `max` (a `@node` port has no static bound)"
                    .into(),
            })? as f32;

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
            node: Box::new(FillSolidNode {
                fill,
                fill_alpha,
                edge,
                edge_width,
                blur_sigma,
                blur_sigma_bound,
                fill_expr,
                opacity_expr,
                fill_expr_src,
                opacity_expr_src,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Solid polygon fill with optional outline and Gaussian blur.",
            "properties": {
                "features": schema_frag::node_ref(),
                "fill": schema_frag::color(),
                "fill-expr": {
                    "description": "A MapLibre color expression (JSON array, e.g. [\"match\", [\"get\", \"class\"], \"water\", \"#88c\", \"#ccc\"] or [\"interpolate\", [\"linear\"], [\"get\", \"area\"], 0, \"#eef\", 1000, \"#049\"]), evaluated per feature group at paint time. When present it overrides the constant `fill`; a group whose expression doesn't resolve to a color is not painted. Unlocks continuous data-driven fills that constant `fill` can't express.",
                },
                "fill-alpha": schema_frag::unit_number(),
                "opacity-expr": {
                    "description": "A MapLibre number expression (JSON array) giving fill opacity, evaluated per feature group at paint time. When present it overrides the constant `fill-alpha`; a group whose expression doesn't resolve to a number falls back to `fill-alpha`.",
                },
                "edge": schema_frag::color(),
                "edge-width": schema_frag::px_number(),
                "blur-sigma": schema_frag::px_number(),
            },
            "required": ["features", "fill"],
        })
    }
}

ezu_graph::submit_node!(FillSolidFactory);
