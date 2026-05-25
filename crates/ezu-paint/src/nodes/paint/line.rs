//! `line` — `Features + Brush -> Raster`. Wraps
//! [`paint_lines`](crate::paint_lines): hokusai brush stroke along
//! polylines with world-seeded pressure jitter.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use hokusai::Brush;
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    canvas_into_raster, core_tile, downcast_brush, downcast_features, empty_raster, make_canvas,
    read_color, read_number, read_number_or, resolve_field, srgb_to_linear_rgba,
};
use crate::{paint_lines, LineStrokeStyle};
// NOTE: `paint_lines_parallel` exists behind the `parallel` feature but
// is intentionally NOT wired in here yet. Without a hokusai-side
// `MemSurface::merge_premul_over` primitive, per-chunk MemSurfaces have
// to be flattened to 8-bit before composite, which loses fix15
// precision relative to the serial path and produces near-but-not
// bit-identical output. See `out/hokusai-parallelization-reply.md`.

struct LineNode {
    color: [f32; 3],
    pressure_base: f32,
    pressure_jitter: f32,
    dtime: f32,
    radius_px: Option<f32>,
    opacity: Option<f32>,
    radius_stroke_curve: Option<Vec<(f32, f32)>>,
    opacity_stroke_curve: Option<Vec<(f32, f32)>>,
    hardness_stroke_curve: Option<Vec<(f32, f32)>>,
    dtime_stroke_curve: Option<Vec<(f32, f32)>>,
}

impl Node for LineNode {
    fn op_name(&self) -> &'static str {
        "line"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[
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
        SPECS
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
        if feats.lines.is_empty() {
            return Ok(empty_raster(ctx));
        }
        let mut canvas = make_canvas(ctx)?;
        // Clone brush and apply optional radius / opacity overrides.
        let mut brush: Brush = (*brush_arc).clone();
        if let Some(r) = self.radius_px {
            brush.get_mut(hokusai::BrushSetting::Radius).base_value = r.max(0.05).ln();
        }
        if let Some(o) = self.opacity {
            brush.get_mut(hokusai::BrushSetting::Opaque).base_value = o.clamp(0.0, 1.0);
        }
        let style = LineStrokeStyle {
            color: self.color,
            pressure_base: self.pressure_base,
            pressure_jitter: self.pressure_jitter,
            dtime: self.dtime,
            radius_stroke_curve: self.radius_stroke_curve.clone(),
            opacity_stroke_curve: self.opacity_stroke_curve.clone(),
            hardness_stroke_curve: self.hardness_stroke_curve.clone(),
            dtime_stroke_curve: self.dtime_stroke_curve.clone(),
        };
        paint_lines(
            &mut canvas,
            &feats.lines,
            feats.extent,
            core_tile(ctx),
            &brush,
            &style,
        );
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"line");
        for c in self.color {
            h.update(&c.to_le_bytes());
        }
        for v in [self.pressure_base, self.pressure_jitter, self.dtime] {
            h.update(&v.to_le_bytes());
        }
        if let Some(r) = self.radius_px {
            h.update(&[1]);
            h.update(&r.to_le_bytes());
        } else {
            h.update(&[0]);
        }
        if let Some(o) = self.opacity {
            h.update(&[1]);
            h.update(&o.to_le_bytes());
        } else {
            h.update(&[0]);
        }
        hash_curve(h, b"r", self.radius_stroke_curve.as_deref());
        hash_curve(h, b"o", self.opacity_stroke_curve.as_deref());
        hash_curve(h, b"h", self.hardness_stroke_curve.as_deref());
        hash_curve(h, b"d", self.dtime_stroke_curve.as_deref());
    }
}

fn hash_curve(h: &mut Xxh3, tag: &[u8], curve: Option<&[(f32, f32)]>) {
    h.update(tag);
    match curve {
        None => h.update(&[0]),
        Some(pts) => {
            h.update(&[1]);
            h.update(&(pts.len() as u32).to_le_bytes());
            for (x, y) in pts {
                h.update(&x.to_le_bytes());
                h.update(&y.to_le_bytes());
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
        let color_srgb = read_color(fields, "color", ctx)?;
        let lin = srgb_to_linear_rgba(color_srgb);
        let color = [lin[0], lin[1], lin[2]];
        let pressure_base = read_number_or(fields, "pressure-base", ctx, 0.7)? as f32;
        let pressure_jitter = read_number_or(fields, "pressure-jitter", ctx, 0.2)? as f32;
        let dtime = read_number_or(fields, "dtime", ctx, 0.02)? as f32;
        let radius_px = if fields.contains_key("radius-px") {
            Some(read_number(fields, "radius-px", ctx)? as f32)
        } else {
            None
        };
        let opacity = if fields.contains_key("opacity") {
            Some(read_number(fields, "opacity", ctx)? as f32)
        } else {
            None
        };
        let radius_stroke_curve = read_stroke_curve(fields, "radius-stroke-curve", ctx)?;
        let opacity_stroke_curve = read_stroke_curve(fields, "opacity-stroke-curve", ctx)?;
        let hardness_stroke_curve = read_stroke_curve(fields, "hardness-stroke-curve", ctx)?;
        let dtime_stroke_curve = read_stroke_curve(fields, "dtime-stroke-curve", ctx)?;
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
            }),
            connections: vec![
                Connection {
                    port: "features".into(),
                    src: features,
                },
                Connection {
                    port: "brush".into(),
                    src: brush,
                },
            ],
        })
    }
    fn schema(&self) -> Value {
        let curve_shape = serde_json::json!({
            "type": "array",
            "items": {
                "type": "array",
                "items": { "type": "number" },
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
                "dtime": { "type": "number", "minimum": 0.0 },
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
    ctx: &FactoryCtx<'_>,
) -> Result<Option<Vec<(f32, f32)>>, FactoryError> {
    if !fields.contains_key(name) {
        return Ok(None);
    }
    let v = resolve_field(fields, name, ctx)?;
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
    let mut prev_t: Option<f32> = None;
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
        let t = pair[0].as_f64().ok_or_else(|| FactoryError::BadField {
            field: name.into(),
            msg: format!("entry {i}: t must be number"),
        })? as f32;
        let y = pair[1].as_f64().ok_or_else(|| FactoryError::BadField {
            field: name.into(),
            msg: format!("entry {i}: y must be number"),
        })? as f32;
        if let Some(p) = prev_t {
            if t < p {
                return Err(FactoryError::BadField {
                    field: name.into(),
                    msg: format!("entry {i}: t must be non-decreasing"),
                });
            }
        }
        prev_t = Some(t);
        out.push((t, y));
    }
    Ok(Some(out))
}

ezu_graph::submit_node!(LineFactory);
