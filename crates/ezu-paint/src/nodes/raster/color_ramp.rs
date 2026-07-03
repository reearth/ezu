//! `color-ramp` — `ScalarField | Raster -> Raster`. Map per-pixel scalar
//! values to colour via a user-supplied stop table, interpolating between
//! stops in a selectable colour `space`; samples outside the range clamp
//! to the end colours. A `Raster` input is recoloured by its luminance
//! (Rec. 601) — a **gradient map** — preserving source coverage.
//!
//! The canonical cartographic use case is **hypsometric tinting** —
//! map an elevation `ScalarField` (from `dem`) to a green→brown→white
//! ramp. The same op works on any scalar field: a `distance_field`
//! mapped to bands, scalar noise mapped to a custom palette, slope
//! angle mapped to colour, etc.
//!
//! An optional `ramp-expr` — a raw MapLibre **color** expression over
//! `heatmap-density` — overrides `stops`: at eval start the expression
//! is sampled at 256 evenly spaced densities in `[0, 1]` into a LUT
//! (mirroring MapLibre's own 256-px ramp texture, so nested
//! zoom×density expressions are faithful), and per-pixel values clamp
//! to `[0, 1]` and interpolate linearly between LUT entries. This is
//! how a `density` field takes a MapLibre `heatmap-color`.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};

use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::color_interp::{interpolate, InterpSpace};
use crate::nodes::common::read_space;

#[derive(Debug, Clone, Copy)]
struct Stop {
    value: f32,
    rgba: [u8; 4],
}

struct ColorRampNode {
    /// Empty iff `ramp_expr` is set (the factory requires one of the two).
    stops: Vec<Stop>,
    space: InterpSpace,
    /// Optional raw MapLibre color expression over `heatmap-density`;
    /// overrides `stops` (see the module docs for the LUT semantics).
    ramp_expr: Option<maplibre_expr::Expr>,
    /// Raw `ramp-expr` JSON text, for a stable hash.
    ramp_expr_src: Option<String>,
    /// Uniform output-alpha multiplier (layer opacity). Feed an `expr`
    /// node for a zoom curve.
    opacity: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for ColorRampNode {
    fn op_name(&self) -> &'static str {
        "color-ramp"
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
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("field".into()))?;
        let opacity = (self.opacity.get(ctx, inputs)? as f32).clamp(0.0, 1.0);
        // With `ramp-expr`, bake the expression into a 256-entry LUT once
        // per eval (zoom is in the context, so zoom×density curves work);
        // otherwise sample the stop table directly.
        let lut = self.ramp_expr.as_ref().map(|e| build_lut(e, ctx.tile.z));
        let sample = |v: f32| -> [u8; 4] {
            match &lut {
                Some(lut) => sample_lut(lut, v),
                None => sample_stops(&self.stops, v, self.space),
            }
        };
        // ScalarField: map each value through the ramp (hypsometric tint).
        if let Some(field) = input.as_scalar_field() {
            let mut out = RasterBuf::new(field.width, field.height);
            for (i, &v) in field.values.iter().enumerate() {
                let rgba = sample(v);
                let off = i * 4;
                // Premultiply alpha to match the rest of the pipeline.
                let af = rgba[3] as f32 / 255.0 * opacity;
                out.pixels[off] = (rgba[0] as f32 * af).round() as u8;
                out.pixels[off + 1] = (rgba[1] as f32 * af).round() as u8;
                out.pixels[off + 2] = (rgba[2] as f32 * af).round() as u8;
                out.pixels[off + 3] = (af * 255.0).round() as u8;
            }
            return Ok(PortValue::Raster(Arc::new(out)));
        }
        // Raster: gradient-map — recolour by per-pixel luminance (Rec. 601)
        // through the stops, preserving source coverage (alpha).
        if let Some(src) = input.as_raster() {
            let mut out = RasterBuf::new(src.width, src.height);
            for i in (0..src.pixels.len()).step_by(4) {
                let a = src.pixels[i + 3] as f32 / 255.0;
                let (r, g, b) = if a > 0.0 {
                    (
                        src.pixels[i] as f32 / 255.0 / a,
                        src.pixels[i + 1] as f32 / 255.0 / a,
                        src.pixels[i + 2] as f32 / 255.0 / a,
                    )
                } else {
                    (0.0, 0.0, 0.0)
                };
                let luma = (0.299 * r + 0.587 * g + 0.114 * b).clamp(0.0, 1.0);
                let rgba = sample(luma);
                let oa = a * (rgba[3] as f32 / 255.0) * opacity;
                out.pixels[i] = (rgba[0] as f32 / 255.0 * oa * 255.0).round() as u8;
                out.pixels[i + 1] = (rgba[1] as f32 / 255.0 * oa * 255.0).round() as u8;
                out.pixels[i + 2] = (rgba[2] as f32 / 255.0 * oa * 255.0).round() as u8;
                out.pixels[i + 3] = (oa * 255.0).round() as u8;
            }
            return Ok(PortValue::Raster(Arc::new(out)));
        }
        Err(EvalError::Other(format!(
            "color-ramp: expected ScalarField or Raster, got {:?}",
            input.kind()
        )))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"color-ramp");
        h.update(&[self.space.hash_tag()]);
        for s in &self.stops {
            h.update(&s.value.to_le_bytes());
            h.update(&s.rgba);
        }
        if let Some(s) = &self.ramp_expr_src {
            h.update(b"rampexpr");
            h.update(s.as_bytes());
        }
        self.opacity.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

/// Bake a `ramp-expr` into 256 straight-RGBA entries by evaluating it at
/// evenly spaced `heatmap-density` values in `[0, 1]` — the same
/// resolution as MapLibre's ramp texture. A sample whose evaluation
/// fails (or isn't a color) becomes transparent black.
fn build_lut(expr: &maplibre_expr::Expr, z: u8) -> Vec<[f32; 4]> {
    let mut ectx = maplibre_expr::EvaluationContext::new().with_zoom(z as f64);
    (0..256)
        .map(|i| {
            ectx.heatmap_density = Some(i as f64 / 255.0);
            match maplibre_expr::evaluate(expr, &ectx) {
                Ok(maplibre_expr::Value::Color(c)) => {
                    [c.r as f32, c.g as f32, c.b as f32, c.a as f32]
                }
                _ => [0.0; 4],
            }
        })
        .collect()
}

/// Look a value up in the density LUT: clamp to `[0, 1]`, then
/// interpolate linearly between the two nearest entries (matching the
/// linear filtering of MapLibre's ramp texture).
fn sample_lut(lut: &[[f32; 4]], v: f32) -> [u8; 4] {
    let t = v.clamp(0.0, 1.0) * 255.0;
    let i0 = t.floor() as usize;
    let i1 = (i0 + 1).min(255);
    let f = t - i0 as f32;
    let (a, b) = (lut[i0], lut[i1]);
    to_u8([
        a[0] + (b[0] - a[0]) * f,
        a[1] + (b[1] - a[1]) * f,
        a[2] + (b[2] - a[2]) * f,
        a[3] + (b[3] - a[3]) * f,
    ])
}

fn sample_stops(stops: &[Stop], v: f32, space: InterpSpace) -> [u8; 4] {
    if v <= stops[0].value {
        return stops[0].rgba;
    }
    if v >= stops[stops.len() - 1].value {
        return stops[stops.len() - 1].rgba;
    }
    let mut lo = &stops[0];
    let mut hi = &stops[stops.len() - 1];
    for w in stops.windows(2) {
        if v >= w[0].value && v <= w[1].value {
            lo = &w[0];
            hi = &w[1];
            break;
        }
    }
    let t = ((v - lo.value) / (hi.value - lo.value)).clamp(0.0, 1.0);
    to_u8(interpolate(to_f32(lo.rgba), to_f32(hi.rgba), t, space))
}

fn to_f32(c: [u8; 4]) -> [f32; 4] {
    [
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
        c[3] as f32 / 255.0,
    ]
}

fn to_u8(c: [f32; 4]) -> [u8; 4] {
    [
        (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[3].clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

pub(super) struct ColorRampFactory;
impl NodeFactory for ColorRampFactory {
    fn op_name(&self) -> &'static str {
        "color-ramp"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "field")?;
        // `ramp-expr`: a raw MapLibre color expression over
        // `heatmap-density`, compiled once. When present it overrides
        // `stops` (which then becomes optional).
        let (ramp_expr, ramp_expr_src) = match fields.get("ramp-expr") {
            Some(v) => {
                let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
                    field: "ramp-expr".into(),
                    msg: e.to_string(),
                })?;
                let expr =
                    maplibre_expr::typecheck(&expr, Some(&maplibre_expr::Type::Color), false)
                        .map_err(|e| FactoryError::BadField {
                            field: "ramp-expr".into(),
                            msg: e.to_string(),
                        })?;
                (Some(expr), Some(v.to_string()))
            }
            None => (None, None),
        };
        let stops = match fields.get("stops") {
            Some(raw) => parse_stops(raw)?,
            None if ramp_expr.is_some() => Vec::new(),
            None => return Err(FactoryError::MissingField("stops".into())),
        };
        let space = read_space(fields)?;
        let mut r = InReader::new(fields, ctx, 1);
        let opacity = r.number_or("opacity", 1.0)?;
        let parts = r.finish();

        let mut ports = vec![PortSpec {
            name: "field",
            accepts: &[PortKind::ScalarField, PortKind::Raster],
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "field".into(),
            src: input,
        }];
        connections.extend(parts.connections);
        Ok(BuiltNode {
            node: Box::new(ColorRampNode {
                stops,
                space,
                ramp_expr,
                ramp_expr_src,
                opacity,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Map a `ScalarField` (or a `Raster`, by its luminance — a gradient map) to colour through a stop table, interpolating between stops in `space`. Samples outside `[stops[0].value, stops[-1].value]` clamp to the end colours. Canonical use case is hypsometric tinting over a DEM (`stops[i].value` = elevation in metres). Give `ramp-expr` instead of `stops` to drive the ramp from a MapLibre color expression over `heatmap-density`.",
            "properties": {
                "field": schema_frag::node_ref(),
                "ramp-expr": {
                    "description": "A MapLibre color expression over `heatmap-density` (e.g. a `heatmap-color` value). When present it overrides `stops`: the expression is baked into a 256-entry LUT per eval (zoom in context), inputs clamp to [0, 1], and lookups interpolate linearly between entries — mirroring MapLibre's 256-px ramp texture. `stops` is only required when this is absent.",
                },
                "stops": {
                    "type": "array",
                    "minItems": 2,
                    "items": {
                        "type": "object",
                        "properties": {
                            "value": { "type": "number", "description": "Scalar value at this stop (e.g. metres of elevation for a DEM field)." },
                            "color": { "type": "string", "description": "`#rrggbb` or `#rrggbbaa`." },
                        },
                        "required": ["value", "color"],
                    },
                },
                "space": { "type": "string", "enum": ["rgb", "hsl", "hsv", "hcl", "lab"], "default": "rgb", "description": "Colour space the stops interpolate in. `rgb` (default) is a straight sRGB lerp; `hsl`/`hsv`/`hcl` interpolate hue on the shortest path; `hcl`/`lab` are perceptual (ported from the MapLibre style spec)." },
                "opacity": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "maximum": 1.0,
                             "description": "Uniform output-alpha multiplier (layer opacity). Default 1.0. Feed an `expr` node via `@node` for a zoom curve." })),
            },
            "required": ["field"],
        })
    }
}

fn parse_stops(raw: &Value) -> Result<Vec<Stop>, FactoryError> {
    let arr = raw.as_array().ok_or_else(|| FactoryError::BadField {
        field: "stops".into(),
        msg: "expected an array of {value, color} objects".into(),
    })?;
    if arr.len() < 2 {
        return Err(FactoryError::BadField {
            field: "stops".into(),
            msg: "at least two stops required".into(),
        });
    }
    let mut stops: Vec<Stop> = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        let obj = v.as_object().ok_or_else(|| FactoryError::BadField {
            field: format!("stops[{i}]"),
            msg: "expected object".into(),
        })?;
        let value =
            obj.get("value")
                .and_then(Value::as_f64)
                .ok_or_else(|| FactoryError::BadField {
                    field: format!("stops[{i}].value"),
                    msg: "expected number".into(),
                })? as f32;
        let color_s =
            obj.get("color")
                .and_then(Value::as_str)
                .ok_or_else(|| FactoryError::BadField {
                    field: format!("stops[{i}].color"),
                    msg: "expected #rrggbb[aa] string".into(),
                })?;
        let rgba = parse_hex_rgba(color_s).ok_or_else(|| FactoryError::BadField {
            field: format!("stops[{i}].color"),
            msg: format!("bad color: {color_s}"),
        })?;
        stops.push(Stop { value, rgba });
    }
    stops.sort_by(|a, b| {
        a.value
            .partial_cmp(&b.value)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(stops)
}

fn parse_hex_rgba(s: &str) -> Option<[u8; 4]> {
    let s = s.strip_prefix('#')?;
    match s.len() {
        6 => Some([
            u8::from_str_radix(&s[0..2], 16).ok()?,
            u8::from_str_radix(&s[2..4], 16).ok()?,
            u8::from_str_radix(&s[4..6], 16).ok()?,
            255,
        ]),
        8 => Some([
            u8::from_str_radix(&s[0..2], 16).ok()?,
            u8::from_str_radix(&s[2..4], 16).ok()?,
            u8::from_str_radix(&s[4..6], 16).ok()?,
            u8::from_str_radix(&s[6..8], 16).ok()?,
        ]),
        _ => None,
    }
}

ezu_graph::submit_node!(ColorRampFactory);
