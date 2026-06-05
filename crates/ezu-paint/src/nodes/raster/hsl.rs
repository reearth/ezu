//! `hsl` — HSL adjustment over `Raster|Sprite` (pass-through). Hue
//! rotation (degrees), saturation and lightness shifts in `[-1, 1]`.
//! Alpha preserved.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    raster_or_sprite_output, unwrap_raster_or_sprite, wrap_raster_like, ACCEPTS_RASTER_OR_SPRITE,
};

struct HslNode {
    hue_shift: In<f64>,  // degrees
    saturation: In<f64>, // -1..1
    lightness: In<f64>,  // -1..1
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for HslNode {
    fn op_name(&self) -> &'static str {
        "hsl"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        raster_or_sprite_output(input_kinds)
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let (src, kind) = unwrap_raster_or_sprite(input, "input")?;
        let hue_shift = self.hue_shift.get(ctx, inputs)? as f32;
        let saturation = self.saturation.get(ctx, inputs)? as f32;
        let lightness = self.lightness.get(ctx, inputs)? as f32;
        let mut out = RasterBuf::new(src.width, src.height);
        for i in (0..src.pixels.len()).step_by(4) {
            let a = src.pixels[i + 3] as f32 / 255.0;
            if a <= 0.0 {
                continue;
            }
            let r = (src.pixels[i] as f32 / 255.0) / a;
            let g = (src.pixels[i + 1] as f32 / 255.0) / a;
            let b = (src.pixels[i + 2] as f32 / 255.0) / a;
            let (mut h, mut s, mut l) = rgb_to_hsl(r.min(1.0), g.min(1.0), b.min(1.0));
            h = (h + hue_shift).rem_euclid(360.0);
            // Saturation/lightness shift toward 0 or 1 by the param amount.
            s = shift_toward(s, saturation);
            l = shift_toward(l, lightness);
            let (nr, ng, nb) = hsl_to_rgb(h, s, l);
            out.pixels[i] = (nr * a * 255.0).round() as u8;
            out.pixels[i + 1] = (ng * a * 255.0).round() as u8;
            out.pixels[i + 2] = (nb * a * 255.0).round() as u8;
            out.pixels[i + 3] = src.pixels[i + 3];
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"hsl");
        self.hue_shift.param_hash(h);
        self.saturation.param_hash(h);
        self.lightness.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct HslFactory;
impl NodeFactory for HslFactory {
    fn op_name(&self) -> &'static str {
        "hsl"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let mut r = InReader::new(fields, ctx, 1);
        let hue_shift = r.number_or("hue-shift", 0.0)?;
        let saturation = r.number_or("saturation", 0.0)?;
        let lightness = r.number_or("lightness", 0.0)?;
        let parts = r.finish();

        let mut ports = vec![PortSpec {
            name: "input",
            accepts: ACCEPTS_RASTER_OR_SPRITE,
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "input".into(),
            src: input,
        }];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(HslNode {
                hue_shift,
                saturation,
                lightness,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "HSL adjustment: rotate hue by `hue-shift` degrees, shift saturation/lightness in [-1, 1] (0 = no change, +1 = toward max, -1 = toward 0).",
            "properties": {
                "input": schema_frag::node_ref(),
                "hue-shift": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.0 })),
                "saturation": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": -1.0, "maximum": 1.0, "default": 0.0 })),
                "lightness": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": -1.0, "maximum": 1.0, "default": 0.0 })),
            },
            "required": ["input"],
        })
    }
}

// ---------------------------------------------------------------------------
// HSL conversions. H in degrees [0, 360), S and L in [0, 1].

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    if (max - min).abs() < 1e-6 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } * 60.0;
    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hh = h / 60.0;
    let x = c * (1.0 - (hh.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hh.floor() as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c * 0.5;
    (
        (r1 + m).clamp(0.0, 1.0),
        (g1 + m).clamp(0.0, 1.0),
        (b1 + m).clamp(0.0, 1.0),
    )
}

/// Move `v` toward 0 (when `t < 0`) or 1 (when `t > 0`) by `|t|`.
fn shift_toward(v: f32, t: f32) -> f32 {
    let t = t.clamp(-1.0, 1.0);
    if t >= 0.0 {
        v + (1.0 - v) * t
    } else {
        v + v * t
    }
}

ezu_graph::submit_node!(HslFactory);
