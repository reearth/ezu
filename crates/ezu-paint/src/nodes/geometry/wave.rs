//! `wave` — `Features -> Features`. Displace each polyline laterally
//! with a sine wave: `amplitude-px` peak deviation, `wavelength-px`
//! period along arc length. Output is a denser polyline that
//! approximates the curve as a chain of straight segments.
//!
//! The local "left" direction is the per-segment perpendicular
//! `(-uy, ux)`; sharp corners may kink (no tangent smoothing across
//! joins). Polygons and points pass through unchanged.

use std::f64::consts::PI;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError,
    FactoryCtx, FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, read_number, read_number_or};

struct WaveNode {
    amplitude_px: f64,
    wavelength_px: f64,
    phase_px: f64,
    samples_per_wavelength: u32,
}

impl Node for WaveNode {
    fn op_name(&self) -> &'static str {
        "wave"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "features",
            kind: PortKind::Features,
            optional: false,
        }];
        SPECS
    }
    fn output(&self) -> PortKind {
        PortKind::Features
    }
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::Tile
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
        let scale = feats.extent as f64 / ctx.canvas.tile_size.max(1) as f64;
        let amp = self.amplitude_px * scale;
        let wavelen = self.wavelength_px * scale;
        let phase = self.phase_px * scale;

        let mut out_lines: Vec<Vec<(i32, i32)>> = Vec::with_capacity(feats.lines.len());
        for line in &feats.lines {
            if let Some(displaced) =
                wave_polyline(line, amp, wavelen, phase, self.samples_per_wavelength)
            {
                out_lines.push(displaced);
            }
        }
        Ok(features_value(
            feats.extent,
            feats.polygons.clone(),
            out_lines,
            feats.points.clone(),
        ))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"wave");
        h.update(&self.amplitude_px.to_le_bytes());
        h.update(&self.wavelength_px.to_le_bytes());
        h.update(&self.phase_px.to_le_bytes());
        h.update(&self.samples_per_wavelength.to_le_bytes());
    }
}

fn wave_polyline(
    line: &[(i32, i32)],
    amp: f64,
    wavelen: f64,
    phase: f64,
    samples_per_wavelen: u32,
) -> Option<Vec<(i32, i32)>> {
    if line.len() < 2 {
        return None;
    }
    if !wavelen.is_finite() || wavelen <= 0.0 || amp == 0.0 {
        return Some(line.to_vec());
    }
    let step = (wavelen / samples_per_wavelen.max(2) as f64).max(0.5);
    let inv_wavelen = 2.0 * PI / wavelen;

    let mut out: Vec<(f64, f64)> = Vec::with_capacity(line.len() * samples_per_wavelen as usize);
    let mut s_total = 0.0;

    let (mut x, mut y) = (line[0].0 as f64, line[0].1 as f64);
    // Emit the start sample with the local tangent of the first segment.
    let first = line[1];
    let dx0 = first.0 as f64 - x;
    let dy0 = first.1 as f64 - y;
    let len0 = (dx0 * dx0 + dy0 * dy0).sqrt().max(1e-9);
    let (mut ux, mut uy) = (dx0 / len0, dy0 / len0);
    let off0 = amp * (inv_wavelen * (s_total + phase)).sin();
    out.push((x + -uy * off0, y + ux * off0));

    for win in line.windows(2) {
        let (xn, yn) = (win[1].0 as f64, win[1].1 as f64);
        let dx = xn - x;
        let dy = yn - y;
        let seg = (dx * dx + dy * dy).sqrt();
        if seg <= 0.0 {
            x = xn;
            y = yn;
            continue;
        }
        ux = dx / seg;
        uy = dy / seg;
        let n_sub = (seg / step).ceil().max(1.0) as usize;
        for k in 1..=n_sub {
            let frac = ((k as f64) / (n_sub as f64)).min(1.0);
            let px = x + dx * frac;
            let py = y + dy * frac;
            let cur_s = s_total + seg * frac;
            let off = amp * (inv_wavelen * (cur_s + phase)).sin();
            out.push((px + -uy * off, py + ux * off));
        }
        s_total += seg;
        x = xn;
        y = yn;
    }
    Some(quantize(&out))
}

fn quantize(pts: &[(f64, f64)]) -> Vec<(i32, i32)> {
    let mut out = Vec::with_capacity(pts.len());
    let mut last: Option<(i32, i32)> = None;
    for &(x, y) in pts {
        let q = (x.round() as i32, y.round() as i32);
        if Some(q) != last {
            out.push(q);
            last = Some(q);
        }
    }
    out
}

pub(super) struct WaveFactory;
impl NodeFactory for WaveFactory {
    fn op_name(&self) -> &'static str {
        "wave"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let amplitude_px = read_number(fields, "amplitude-px", ctx)?;
        let wavelength_px = read_number(fields, "wavelength-px", ctx)?;
        let phase_px = read_number_or(fields, "phase-px", ctx, 0.0)?;
        let samples_per_wavelength = read_number_or(fields, "samples-per-wavelength", ctx, 16.0)?
            .clamp(2.0, 256.0) as u32;
        Ok(BuiltNode {
            node: Box::new(WaveNode {
                amplitude_px,
                wavelength_px,
                phase_px,
                samples_per_wavelength,
            }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Displace polylines laterally with a sine wave. Lengths in canvas pixels. Sharp corners may kink (no tangent smoothing). Polygons/points pass through.",
            "properties": {
                "features": schema_frag::node_ref(),
                "amplitude-px": { "type": "number",
                                  "description": "Peak lateral deviation in pixels. May be negative to flip phase." },
                "wavelength-px": { "type": "number", "minimum": 0.5 },
                "phase-px": { "type": "number",
                              "description": "Offset into the wave at the polyline start, in pixels." },
                "samples-per-wavelength": { "type": "number", "minimum": 2, "maximum": 256,
                                            "description": "Subdivisions per wavelength. Higher = smoother but more vertices. Default 16." },
            },
            "required": ["features", "amplitude-px", "wavelength-px"],
        })
    }
}

ezu_graph::submit_node!(WaveFactory);
