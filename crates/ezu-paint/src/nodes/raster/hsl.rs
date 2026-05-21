//! `hsl` — `Raster -> Raster`. HSL adjustment: hue rotation (degrees),
//! saturation and lightness shifts in `[-1, 1]`. Alpha preserved.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::read_number_or;

struct HslNode {
    hue_shift: f32,  // degrees
    saturation: f32, // -1..1
    lightness: f32,  // -1..1
}

impl Node for HslNode {
    fn op_name(&self) -> &'static str {
        "hsl"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "input",
            kind: PortKind::Raster,
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
        let src = inputs[0]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
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
            h = (h + self.hue_shift).rem_euclid(360.0);
            // Saturation/lightness shift toward 0 or 1 by the param amount.
            s = shift_toward(s, self.saturation);
            l = shift_toward(l, self.lightness);
            let (nr, ng, nb) = hsl_to_rgb(h, s, l);
            out.pixels[i] = (nr * a * 255.0).round() as u8;
            out.pixels[i + 1] = (ng * a * 255.0).round() as u8;
            out.pixels[i + 2] = (nb * a * 255.0).round() as u8;
            out.pixels[i + 3] = src.pixels[i + 3];
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"hsl");
        h.update(&self.hue_shift.to_le_bytes());
        h.update(&self.saturation.to_le_bytes());
        h.update(&self.lightness.to_le_bytes());
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
        let hue_shift = read_number_or(fields, "hue-shift", ctx, 0.0)? as f32;
        let saturation = read_number_or(fields, "saturation", ctx, 0.0)? as f32;
        let lightness = read_number_or(fields, "lightness", ctx, 0.0)? as f32;
        Ok(BuiltNode {
            node: Box::new(HslNode {
                hue_shift,
                saturation,
                lightness,
            }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "HSL adjustment: rotate hue by `hue-shift` degrees, shift saturation/lightness in [-1, 1] (0 = no change, +1 = toward max, -1 = toward 0).",
            "properties": {
                "input": schema_frag::node_ref(),
                "hue-shift": { "type": "number", "default": 0.0 },
                "saturation": { "type": "number", "minimum": -1.0, "maximum": 1.0, "default": 0.0 },
                "lightness": { "type": "number", "minimum": -1.0, "maximum": 1.0, "default": 0.0 },
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
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
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
