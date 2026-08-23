//! `line` — `Features + Brush -> Raster`. Wraps
//! [`paint_lines`](crate::paint_lines): hokusai brush stroke along
//! polylines with world-seeded pressure jitter.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, InfluenceCtx, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use hokusai::Brush;
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    canvas_into_raster, core_tile, downcast_brush, downcast_features, empty_raster, make_canvas,
    srgb_to_linear_rgba,
};
use crate::{paint_lines, LineStrokeStyle};
// NOTE: `paint_lines_parallel` exists behind the `parallel` feature but
// is intentionally NOT wired in here yet. Without a hokusai-side
// `MemSurface::merge_premul_over` primitive, per-chunk MemSurfaces have
// to be flattened to 8-bit before composite, which loses fix15
// precision relative to the serial path and produces near-but-not
// bit-identical output. See `out/hokusai-parallelization-reply.md`.

struct LineNode {
    color: In<[f32; 4]>,
    pressure_base: In<f64>,
    pressure_jitter: In<f64>,
    dtime: In<f64>,
    radius_px: Option<In<f64>>,
    opacity: Option<In<f64>>,
    radius_stroke_curve: Option<CurveIn>,
    opacity_stroke_curve: Option<CurveIn>,
    hardness_stroke_curve: Option<CurveIn>,
    dtime_stroke_curve: Option<CurveIn>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for LineNode {
    fn op_name(&self) -> &'static str {
        "line"
    }

    /// A stroke lays ink a dab's reach away from its path, so geometry
    /// that far outside the canvas still marks it. The reach belongs to
    /// the brush, which arrives on a port — the graph resolves it and
    /// passes it in. `radius-px` replaces the brush's own radius, and a
    /// radius curve lifts it further, so either without a static
    /// ceiling leaves the reach unbounded.
    fn influence_pad(&self, ctx: &InfluenceCtx<'_>) -> u32 {
        let Some(ink) = ctx.brush else {
            return InfluenceCtx::UNBOUNDED;
        };
        // `radius-px` replaces the brush's own radius; the rest of the
        // dab scales with it.
        let mut reach = match &self.radius_px {
            None => ink.reach_px,
            Some(r) => match r.static_bound() {
                Some(b) => ink.at_radius(b),
                None => return InfluenceCtx::UNBOUNDED,
            },
        };
        if let Some(curve) = &self.radius_stroke_curve {
            // A curve's `y` is added to the radius in log space, so its
            // highest knot scales the dab by `e^y`.
            let Some(lift) = curve_max_y(curve) else {
                return InfluenceCtx::UNBOUNDED;
            };
            reach *= lift.max(0.0).exp();
        }
        ctx.plus(reach)
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
        let brush_arc = downcast_brush(
            inputs[1]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("brush".into()))?,
        )?;
        if !feats.has_lines() {
            return Ok(empty_raster(ctx));
        }
        let mut canvas = make_canvas(ctx)?;
        // Clone brush and apply optional radius / opacity overrides.
        let mut brush: Brush = (*brush_arc).clone();
        if let Some(r) = &self.radius_px {
            let r = r.get(ctx, inputs)? as f32;
            brush.get_mut(hokusai::BrushSetting::Radius).base_value = r.max(0.05).ln();
        }
        if let Some(o) = &self.opacity {
            let o = o.get(ctx, inputs)? as f32;
            brush.get_mut(hokusai::BrushSetting::Opaque).base_value = o.clamp(0.0, 1.0);
        }
        let lin = srgb_to_linear_rgba(self.color.get(ctx, inputs)?);
        let style = LineStrokeStyle {
            color: [lin[0], lin[1], lin[2]],
            pressure_base: self.pressure_base.get(ctx, inputs)? as f32,
            pressure_jitter: self.pressure_jitter.get(ctx, inputs)? as f32,
            dtime: self.dtime.get(ctx, inputs)? as f32,
            radius_stroke_curve: resolve_curve(&self.radius_stroke_curve, ctx, inputs)?,
            opacity_stroke_curve: resolve_curve(&self.opacity_stroke_curve, ctx, inputs)?,
            hardness_stroke_curve: resolve_curve(&self.hardness_stroke_curve, ctx, inputs)?,
            dtime_stroke_curve: resolve_curve(&self.dtime_stroke_curve, ctx, inputs)?,
        };
        let lines: Vec<_> = feats.lines().cloned().collect();
        paint_lines(
            &mut canvas,
            &lines,
            feats.extent,
            core_tile(ctx),
            &brush,
            &style,
        );
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"line");
        self.color.param_hash(h);
        self.pressure_base.param_hash(h);
        self.pressure_jitter.param_hash(h);
        self.dtime.param_hash(h);
        if let Some(r) = &self.radius_px {
            h.update(&[1]);
            r.param_hash(h);
        } else {
            h.update(&[0]);
        }
        if let Some(o) = &self.opacity {
            h.update(&[1]);
            o.param_hash(h);
        } else {
            h.update(&[0]);
        }
        hash_curve(h, b"r", self.radius_stroke_curve.as_deref());
        hash_curve(h, b"o", self.opacity_stroke_curve.as_deref());
        hash_curve(h, b"h", self.hardness_stroke_curve.as_deref());
        hash_curve(h, b"d", self.dtime_stroke_curve.as_deref());
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

/// A brush-dynamics curve whose points may each be a `$param`, so the
/// curve is resolved once per eval by [`resolve_curve`].
type CurveIn = Vec<(In<f64>, In<f64>)>;

/// The highest `y` a curve can take, or `None` when a knot has no
/// static ceiling and the curve's effect cannot be bounded.
fn curve_max_y(curve: &CurveIn) -> Option<f64> {
    curve
        .iter()
        .try_fold(f64::MIN, |acc, (_, y)| Some(acc.max(y.static_bound()?)))
}

/// Resolve one curve for an eval. Hokusai wants the points ordered, and
/// a `$param` position is only known now, so sort here.
fn resolve_curve(
    curve: &Option<CurveIn>,
    ctx: &EvalCtx<'_>,
    inputs: &[Option<PortValue>],
) -> Result<Option<Vec<(f32, f32)>>, EvalError> {
    let Some(pts) = curve else { return Ok(None) };
    let mut out = Vec::with_capacity(pts.len());
    for (x, y) in pts {
        out.push((x.get(ctx, inputs)? as f32, y.get(ctx, inputs)? as f32));
    }
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(Some(out))
}

fn hash_curve(h: &mut Xxh3, tag: &[u8], curve: Option<&[(In<f64>, In<f64>)]>) {
    h.update(tag);
    match curve {
        None => h.update(&[0]),
        Some(pts) => {
            h.update(&[1]);
            h.update(&(pts.len() as u32).to_le_bytes());
            for (x, y) in pts {
                x.param_hash(h);
                y.param_hash(h);
            }
        }
    }
}

pub(super) struct LineFactory;
impl NodeFactory for LineFactory {
    fn op_name(&self) -> &'static str {
        "line"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let brush = take_input_ref(fields, "brush")?;
        let mut r = InReader::new(fields, ctx, 2);
        let color = r.color("color")?;
        let pressure_base = r.number_or("pressure-base", 0.7)?;
        let pressure_jitter = r.number_or("pressure-jitter", 0.2)?;
        let dtime = r.number_or("dtime", 0.02)?;
        let radius_px = if fields.contains_key("radius-px") {
            Some(r.number("radius-px")?)
        } else {
            None
        };
        let opacity = if fields.contains_key("opacity") {
            Some(r.number("opacity")?)
        } else {
            None
        };
        let radius_stroke_curve = read_stroke_curve(fields, "radius-stroke-curve", &mut r)?;
        let opacity_stroke_curve = read_stroke_curve(fields, "opacity-stroke-curve", &mut r)?;
        let hardness_stroke_curve = read_stroke_curve(fields, "hardness-stroke-curve", &mut r)?;
        let dtime_stroke_curve = read_stroke_curve(fields, "dtime-stroke-curve", &mut r)?;
        let parts = r.finish();

        let mut ports = vec![
            PortSpec {
                name: "features",
                accepts: &[PortKind::Features],
                optional: false,
            },
            PortSpec {
                name: "brush",
                accepts: &[PortKind::Brush],
                optional: false,
            },
        ];
        ports.extend(parts.ports);
        let mut connections = vec![
            Connection {
                port: "features".into(),
                src: features,
            },
            Connection {
                port: "brush".into(),
                src: brush,
            },
        ];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(LineNode {
                color,
                pressure_base,
                pressure_jitter,
                dtime,
                radius_px,
                opacity,
                radius_stroke_curve,
                opacity_stroke_curve,
                hardness_stroke_curve,
                dtime_stroke_curve,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        let curve_shape = serde_json::json!({
            "type": "array",
            "items": {
                "type": "array",
                "items": schema_frag::nested_number(serde_json::json!({ "type": "number" })),
                "minItems": 2,
                "maxItems": 2,
            },
            "minItems": 2,
        });
        let brush_curve = {
            let mut v = curve_shape.clone();
            v["description"] = Value::String(
                "Piecewise-linear `[[t, y], ...]` driving a libmypaint `stroke` input on the brush. `t` is normalized stroke progress in [0, 1]; `y` is an offset added to the setting's base value. `radius` is log-space (y=-2.3 ≈ ×0.1, y=+0.69 ≈ ×2); `opaque` and `hardness` are linear."
                    .into(),
            );
            v
        };
        let dtime_curve = {
            let mut v = curve_shape;
            v["description"] = Value::String(
                "Piecewise-linear `[[t, y], ...]` multiplier on `dtime`. `y` scales the per-vertex dtime — y=3 makes the brush linger 3× longer (slower hand), y=0.3 sweeps through 3× faster. Used with dynamics-driven brushes that respond to stroke speed."
                    .into(),
            );
            v
        };
        serde_json::json!({
            "description": "Brush stroke along MVT polylines.",
            "properties": {
                "features": schema_frag::node_ref(),
                "brush": schema_frag::node_ref(),
                "color": schema_frag::color(),
                "radius-px": schema_frag::px_number(),
                "opacity": schema_frag::unit_number(),
                "pressure-base": schema_frag::unit_number(),
                "pressure-jitter": schema_frag::unit_number(),
                "dtime": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0 })),
                "radius-stroke-curve": brush_curve.clone(),
                "opacity-stroke-curve": brush_curve.clone(),
                "hardness-stroke-curve": brush_curve,
                "dtime-stroke-curve": dtime_curve,
            },
            "required": ["features", "brush", "color"],
        })
    }
}

fn read_stroke_curve(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    r: &mut InReader<'_, '_>,
) -> Result<Option<CurveIn>, FactoryError> {
    let Some(v) = fields.get(name) else {
        return Ok(None);
    };
    let arr = v.as_array().ok_or_else(|| FactoryError::BadField {
        field: name.into(),
        msg: "expected array of [t, y] pairs".into(),
    })?;
    if arr.len() < 2 {
        return Err(FactoryError::BadField {
            field: name.into(),
            msg: "stroke curve needs at least 2 points".into(),
        });
    }
    let mut out = Vec::with_capacity(arr.len());
    for (i, pt) in arr.iter().enumerate() {
        let pair = pt.as_array().ok_or_else(|| FactoryError::BadField {
            field: name.into(),
            msg: format!("entry {i}: expected [t, y] pair"),
        })?;
        if pair.len() != 2 {
            return Err(FactoryError::BadField {
                field: name.into(),
                msg: format!("entry {i}: expected exactly 2 numbers"),
            });
        }
        // Ordering is not checked here: a `$param` position is only known
        // at eval, where `resolve_curve` sorts.
        out.push((
            r.nested(&format!("{name}[{i}][0]"), &pair[0])?,
            r.nested(&format!("{name}[{i}][1]"), &pair[1])?,
        ));
    }
    Ok(Some(out))
}

ezu_graph::submit_node!(LineFactory);
