//! `hypsometric` — `HeightField -> Raster`. Map elevation to colour via
//! a user-supplied stop table. Linear interpolation between stops;
//! samples outside the range clamp to the end colours.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};

use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

#[derive(Debug, Clone, Copy)]
struct Stop {
    elev: f32,
    rgba: [u8; 4],
}

struct HypsometricNode {
    stops: Vec<Stop>,
}

impl Node for HypsometricNode {
    fn op_name(&self) -> &'static str {
        "hypsometric"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "field",
            kind: PortKind::HeightField,
            optional: false,
        }];
        SPECS
    }
    fn output(&self) -> PortKind {
        PortKind::Raster
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let field = inputs[0]
            .as_ref()
            .and_then(PortValue::as_height_field)
            .ok_or_else(|| EvalError::MissingInput("field".into()))?;
        let w = field.width;
        let h = field.height;
        let mut out = RasterBuf::new(w, h);
        for (i, &z) in field.elev.iter().enumerate() {
            let rgba = sample_stops(&self.stops, z);
            let off = i * 4;
            // Premultiply alpha to match the rest of the pipeline.
            let af = rgba[3] as f32 / 255.0;
            out.pixels[off] = (rgba[0] as f32 * af).round() as u8;
            out.pixels[off + 1] = (rgba[1] as f32 * af).round() as u8;
            out.pixels[off + 2] = (rgba[2] as f32 * af).round() as u8;
            out.pixels[off + 3] = rgba[3];
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"hypsometric");
        for s in &self.stops {
            h.update(&s.elev.to_le_bytes());
            h.update(&s.rgba);
        }
    }
}

fn sample_stops(stops: &[Stop], z: f32) -> [u8; 4] {
    if z <= stops[0].elev {
        return stops[0].rgba;
    }
    if z >= stops[stops.len() - 1].elev {
        return stops[stops.len() - 1].rgba;
    }
    let mut lo = &stops[0];
    let mut hi = &stops[stops.len() - 1];
    for w in stops.windows(2) {
        if z >= w[0].elev && z <= w[1].elev {
            lo = &w[0];
            hi = &w[1];
            break;
        }
    }
    let t = ((z - lo.elev) / (hi.elev - lo.elev)).clamp(0.0, 1.0);
    [
        lerp(lo.rgba[0], hi.rgba[0], t),
        lerp(lo.rgba[1], hi.rgba[1], t),
        lerp(lo.rgba[2], hi.rgba[2], t),
        lerp(lo.rgba[3], hi.rgba[3], t),
    ]
}

#[inline]
fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

pub(super) struct HypsometricFactory;
impl NodeFactory for HypsometricFactory {
    fn op_name(&self) -> &'static str {
        "hypsometric"
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
            msg: "expected an array of {elev, color} objects".into(),
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
            let elev =
                obj.get("elev")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| FactoryError::BadField {
                        field: format!("stops[{i}].elev"),
                        msg: "expected number (metres)".into(),
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
            stops.push(Stop { elev, rgba });
        }
        stops.sort_by(|a, b| {
            a.elev
                .partial_cmp(&b.elev)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(BuiltNode {
            node: Box::new(HypsometricNode { stops }),
            connections: vec![Connection {
                port: "field".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Map elevation (metres) to colour through a stop table. Samples outside `[stops[0].elev, stops[-1].elev]` clamp to the end colours.",
            "properties": {
                "field": schema_frag::node_ref(),
                "stops": {
                    "type": "array",
                    "minItems": 2,
                    "items": {
                        "type": "object",
                        "properties": {
                            "elev": { "type": "number", "description": "Elevation in metres." },
                            "color": { "type": "string", "description": "`#rrggbb` or `#rrggbbaa`." },
                        },
                        "required": ["elev", "color"],
                    },
                },
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

ezu_graph::submit_node!(HypsometricFactory);
