//! `density` — `Features -> ScalarField`. Kernel-density estimate of the
//! input's point features: every point splats a Gaussian kernel into a
//! per-pixel f32 grid at the padded canvas size. The kernel matches the
//! MapLibre GL JS heatmap shader (quad extent = `radius`, falloff
//! `exp(-0.5 * 3² * (d/radius)²)` scaled by `1/√(2π)`), so a
//! `density` → `color-ramp` chain reproduces a `heatmap` layer.
//!
//! Points are NOT culled against `[0, extent]`: MVT buffer features
//! outside the tile proper still contribute, so the accumulated field —
//! and anything derived from it (`color-ramp`, `contour`) — matches at
//! tile borders. Output values are the raw accumulated density,
//! unclamped; consumers clamp.
//!
//! `radius` is an `In<f64>` field, but pad is computed at build time —
//! so it must carry a static upper bound (a literal, or a `$param` with
//! `max`, or a `radius-max` beside an `@node` port), exactly like
//! `blur`'s sigma. A per-feature `radius-expr` is
//! clamped to that bound at eval time so the pad stays sound.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PaddingIn, PortKind, PortSpec, PortValue,
    ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::downcast_features;

/// `1/√(2π)` — the Gaussian normalisation constant the maplibre-gl-js
/// heatmap shader bakes into each point's contribution.
const GAUSS_COEF: f32 = 0.398_942_3;

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

struct DensityNode {
    radius: PaddingIn,
    intensity: In<f64>,
    /// Build-time upper bound on the kernel radius, for pad propagation.
    /// Per-group `radius-expr` results are clamped to it.
    radius_bound: f32,
    /// Optional data-driven radius / weight / intensity: MapLibre number
    /// expressions evaluated per feature group (zoom is in the context,
    /// so zoom curves work). `weight` has no constant counterpart — a
    /// group without `weight-expr` weighs 1.
    radius_expr: Option<maplibre_expr::Expr>,
    weight_expr: Option<maplibre_expr::Expr>,
    intensity_expr: Option<maplibre_expr::Expr>,
    /// Raw `*-expr` JSON text, for a stable hash.
    radius_expr_src: Option<String>,
    weight_expr_src: Option<String>,
    intensity_expr_src: Option<String>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for DensityNode {
    fn op_name(&self) -> &'static str {
        "density"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::ScalarField
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + self.radius_bound.ceil() as u32
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

        let (pw, ph) = ctx.canvas.padded_dims();
        let mut values = vec![0.0f32; (pw * ph) as usize];

        // Constants, resolved once. Data-driven exprs (if present) override
        // these per feature group; whichever expr is absent uses the constant.
        let const_radius = (self.radius.get(ctx, inputs)? as f32).clamp(0.0, self.radius_bound);
        let const_intensity = (self.intensity.get(ctx, inputs)? as f32).max(0.0);

        let pad = ctx.canvas.pad as f32;
        let tile = ctx.canvas.tile_w as f32;
        let extent = feats.extent.max(1) as f32;
        let sx = tile / extent;
        let z = ctx.tile.z;

        let any_expr = self.radius_expr.is_some()
            || self.weight_expr.is_some()
            || self.intensity_expr.is_some();
        for group in &feats.groups {
            if group.points.is_empty() {
                continue;
            }
            let (radius, amplitude) = if any_expr {
                let ectx = crate::render::group_expr_context(group, z);
                // Evaluated radii clamp to the build-time bound so the
                // pad promised by `required_pad` stays sound.
                let radius = eval_number(&self.radius_expr, &ectx, const_radius)
                    .clamp(0.0, self.radius_bound);
                let weight = eval_number(&self.weight_expr, &ectx, 1.0).max(0.0);
                let intensity = eval_number(&self.intensity_expr, &ectx, const_intensity).max(0.0);
                (radius, weight * intensity)
            } else {
                (const_radius, const_intensity)
            };
            if radius <= 0.0 || amplitude <= 0.0 {
                continue;
            }
            for &(x, y) in &group.points {
                // Tile-local extent units → canvas px, exactly like `stamp`.
                // No culling: points in the MVT buffer splat their in-canvas
                // part so tile borders match the neighbours'.
                let px = x as f32 * sx + pad;
                let py = y as f32 * sx + pad;
                splat(&mut values, pw, ph, px, py, radius, amplitude);
            }
        }

        Ok(PortValue::ScalarField(Arc::new(ScalarField {
            width: pw,
            height: ph,
            values: values.into(),
            nodata: None,
            geo_scale: None,
        })))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"density");
        self.radius.param_hash(h);
        self.intensity.param_hash(h);
        for (tag, src) in [
            (b"radiusexpr".as_slice(), &self.radius_expr_src),
            (b"weightexpr".as_slice(), &self.weight_expr_src),
            (b"intensityexpr".as_slice(), &self.intensity_expr_src),
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

/// Accumulate one point's kernel into the grid, sampling at pixel
/// centers. Mirrors the maplibre-gl-js heatmap shader: the kernel's
/// support is exactly `radius` px and the falloff is
/// `GAUSS_COEF * exp(-0.5 * 3² * (d/radius)²)`.
fn splat(values: &mut [f32], w: u32, h: u32, px: f32, py: f32, radius: f32, amplitude: f32) {
    let x0 = (px - radius - 0.5).floor().max(0.0) as u32;
    let y0 = (py - radius - 0.5).floor().max(0.0) as u32;
    let x1 = ((px + radius - 0.5).ceil().max(0.0) as u32).min(w.saturating_sub(1));
    let y1 = ((py + radius - 0.5).ceil().max(0.0) as u32).min(h.saturating_sub(1));
    if x0 > x1 || y0 > y1 {
        return;
    }
    let inv_r2 = 1.0 / (radius * radius);
    for y in y0..=y1 {
        let dy = (y as f32 + 0.5) - py;
        for x in x0..=x1 {
            let dx = (x as f32 + 0.5) - px;
            let d2 = (dx * dx + dy * dy) * inv_r2;
            if d2 >= 1.0 {
                continue;
            }
            values[(y * w + x) as usize] += amplitude * GAUSS_COEF * (-0.5 * 9.0 * d2).exp();
        }
    }
}

pub(super) struct DensityFactory;
impl NodeFactory for DensityFactory {
    fn op_name(&self) -> &'static str {
        "density"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let mut r = InReader::new(fields, ctx, 1);
        let radius = PaddingIn::read_or(&mut r, fields, "radius", 30.0)?;
        let intensity = r.number_or("intensity", 1.0)?;
        let parts = r.finish();
        let radius_bound = radius.bound() as f32;

        let (radius_expr, radius_expr_src) =
            parse_expr_field(fields, "radius-expr", &maplibre_expr::Type::Number)?;
        let (weight_expr, weight_expr_src) =
            parse_expr_field(fields, "weight-expr", &maplibre_expr::Type::Number)?;
        let (intensity_expr, intensity_expr_src) =
            parse_expr_field(fields, "intensity-expr", &maplibre_expr::Type::Number)?;

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
            node: Box::new(DensityNode {
                radius,
                intensity,
                radius_bound,
                radius_expr,
                weight_expr,
                intensity_expr,
                radius_expr_src,
                weight_expr_src,
                intensity_expr_src,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Kernel-density estimate of the input's point features as a ScalarField (the MapLibre heatmap kernel: support = `radius` px, falloff `exp(-0.5·3²·(d/radius)²)/√(2π)`). Buffer points outside the tile still contribute, so tile borders match. Output is raw accumulated density, unclamped — map it with `color-ramp` or extract isolines with `contour`. Grows upstream pad by `radius`.",
            "properties": {
                "features": schema_frag::node_ref(),
                "radius": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "default": 30,
                "radius-max": { "type": "number", "minimum": 0.0, "description": "Upper bound on `radius` for padding, required when `radius` is an `@node` port. Values above it are clamped." },
                            "description": "Kernel radius in px. Pad is computed at build time, so this needs a static bound: a literal, or a `$param` with `max`." })),
                "radius-expr": {
                    "description": "A MapLibre number expression (px), evaluated per feature group; overrides the constant `radius`. Evaluated values are clamped to `radius`'s static bound so the pad stays sound.",
                },
                "weight-expr": {
                    "description": "A MapLibre number expression giving a point's weight, evaluated per feature group. A group without it (or whose expression doesn't resolve to a number) weighs 1.",
                },
                "intensity": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "default": 1,
                               "description": "Global multiplier on every point's contribution." })),
                "intensity-expr": {
                    "description": "A MapLibre number expression giving the intensity multiplier, evaluated per feature group; overrides the constant `intensity`.",
                },
            },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(DensityFactory);
