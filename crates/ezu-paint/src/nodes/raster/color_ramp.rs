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

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
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
    stops: Vec<Stop>,
    space: InterpSpace,
}

impl Node for ColorRampNode {
    fn op_name(&self) -> &'static str {
        "color-ramp"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "field",
            accepts: &[PortKind::ScalarField, PortKind::Raster],
            optional: false,
        }];
        SPECS
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("field".into()))?;
        // ScalarField: map each value through the stops (hypsometric tint).
        if let Some(field) = input.as_scalar_field() {
            let mut out = RasterBuf::new(field.width, field.height);
            for (i, &v) in field.values.iter().enumerate() {
                let rgba = sample_stops(&self.stops, v, self.space);
                let off = i * 4;
                // Premultiply alpha to match the rest of the pipeline.
                let af = rgba[3] as f32 / 255.0;
                out.pixels[off] = (rgba[0] as f32 * af).round() as u8;
                out.pixels[off + 1] = (rgba[1] as f32 * af).round() as u8;
                out.pixels[off + 2] = (rgba[2] as f32 * af).round() as u8;
                out.pixels[off + 3] = rgba[3];
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
                let rgba = sample_stops(&self.stops, luma, self.space);
                let oa = a * (rgba[3] as f32 / 255.0);
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
    }
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
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "field")?;
        let raw = fields
            .get("stops")
            .ok_or_else(|| FactoryError::MissingField("stops".into()))?;
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
        let space = read_space(fields)?;
        Ok(BuiltNode {
            node: Box::new(ColorRampNode { stops, space }),
            connections: vec![Connection {
                port: "field".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Map a `ScalarField` (or a `Raster`, by its luminance — a gradient map) to colour through a stop table, interpolating between stops in `space`. Samples outside `[stops[0].value, stops[-1].value]` clamp to the end colours. Canonical use case is hypsometric tinting over a DEM (`stops[i].value` = elevation in metres).",
            "properties": {
                "field": schema_frag::node_ref(),
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
            },
            "required": ["field", "stops"],
        })
    }
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
