//! `wave` — `Features -> Features`. Displace each polyline laterally
//! with a sine wave: `amplitude-px` peak deviation, `wavelength-px`
//! period along arc length. Output is a denser polyline that
//! approximates the curve as a chain of straight segments.
//!
//! Optional `noise-amp-px` adds a smooth 2D value-noise displacement
//! on top of the sine, with cell size `noise-scale-px` (defaults to
//! `wavelength-px`). The noise is sampled in WORLD-pixel coordinates,
//! so adjacent tiles agree on the displacement at the seam and the
//! jittered line meets cleanly across tile borders. Use it to break
//! the regularity of the sine — a small ratio
//! (noise-amp ≈ 0.3·amplitude) reads as "shaky hand".
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
    noise_amp_px: f64,
    noise_scale_px: f64,
    seed: Option<u32>,
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
        let noise_amp = self.noise_amp_px * scale;
        let noise_scale = self.noise_scale_px * scale;
        // Use a constant default seed so the world-anchored noise is
        // deterministic across tiles. Per-tile rng_seed would make the
        // noise pattern shift at every tile border.
        let seed = self.seed.unwrap_or(0xA17F_B91D);
        // World origin of this tile in feature-extent units, so the
        // noise field is the same global function across tiles.
        let extent = feats.extent as f64;
        let origin_x = ctx.tile.x as f64 * extent;
        let origin_y = ctx.tile.y as f64 * extent;

        let mut out_lines: Vec<Vec<(i32, i32)>> = Vec::with_capacity(feats.lines.len());
        for line in feats.lines.iter() {
            if let Some(displaced) = wave_polyline(
                line,
                amp,
                wavelen,
                phase,
                self.samples_per_wavelength,
                noise_amp,
                noise_scale,
                seed,
                (origin_x, origin_y),
            ) {
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
        h.update(&self.noise_amp_px.to_le_bytes());
        h.update(&self.noise_scale_px.to_le_bytes());
        if let Some(s) = self.seed {
            h.update(b"seed");
            h.update(&s.to_le_bytes());
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn wave_polyline(
    line: &[(i32, i32)],
    amp: f64,
    wavelen: f64,
    phase: f64,
    samples_per_wavelen: u32,
    noise_amp: f64,
    noise_scale: f64,
    seed: u32,
    world_origin: (f64, f64),
) -> Option<Vec<(i32, i32)>> {
    if line.len() < 2 {
        return None;
    }
    let sine_active = wavelen.is_finite() && wavelen > 0.0 && amp != 0.0;
    let noise_active = noise_scale.is_finite() && noise_scale > 0.0 && noise_amp != 0.0;
    if !sine_active && !noise_active {
        return Some(line.to_vec());
    }
    // Step from whichever wavelength is shorter so both components are
    // sampled densely enough.
    let driving_wavelen = if sine_active && noise_active {
        wavelen.min(noise_scale)
    } else if sine_active {
        wavelen
    } else {
        noise_scale
    };
    let step = (driving_wavelen / samples_per_wavelen.max(2) as f64).max(0.5);
    let inv_wavelen = if sine_active { 2.0 * PI / wavelen } else { 0.0 };
    let inv_noise_scale = if noise_active { 1.0 / noise_scale } else { 0.0 };

    let mut out: Vec<(f64, f64)> = Vec::with_capacity(line.len() * samples_per_wavelen as usize);
    let mut s_total = 0.0;

    let (ox, oy) = world_origin;
    let (mut x, mut y) = (line[0].0 as f64, line[0].1 as f64);
    // Emit the start sample with the local tangent of the first segment.
    let first = line[1];
    let dx0 = first.0 as f64 - x;
    let dy0 = first.1 as f64 - y;
    let len0 = (dx0 * dx0 + dy0 * dy0).sqrt().max(1e-9);
    let (mut ux, mut uy) = (dx0 / len0, dy0 / len0);
    let off0 = offset_at(
        s_total,
        amp,
        inv_wavelen,
        phase,
        noise_amp,
        inv_noise_scale,
        seed,
        x + ox,
        y + oy,
    );
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
            let off = offset_at(
                cur_s,
                amp,
                inv_wavelen,
                phase,
                noise_amp,
                inv_noise_scale,
                seed,
                px + ox,
                py + oy,
            );
            out.push((px + -uy * off, py + ux * off));
        }
        s_total += seg;
        x = xn;
        y = yn;
    }
    Some(quantize(&out))
}

#[allow(clippy::too_many_arguments)]
fn offset_at(
    s: f64,
    amp: f64,
    inv_wavelen: f64,
    phase: f64,
    noise_amp: f64,
    inv_noise_scale: f64,
    seed: u32,
    world_x: f64,
    world_y: f64,
) -> f64 {
    let sine = if amp != 0.0 && inv_wavelen != 0.0 {
        amp * (inv_wavelen * (s + phase)).sin()
    } else {
        0.0
    };
    let noise = if noise_amp != 0.0 && inv_noise_scale != 0.0 {
        // Map [0,1] → [-1,1] then scale. Sample in world space so the
        // jitter is the same global function across tiles.
        noise_amp
            * (value_noise_2d(world_x * inv_noise_scale, world_y * inv_noise_scale, seed) * 2.0
                - 1.0)
    } else {
        0.0
    };
    sine + noise
}

fn hash_u32(x: u32) -> u32 {
    let mut h = x.wrapping_mul(0x9E37_79B9);
    h ^= h >> 16;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 16)
}

fn hash2(ix: i64, iy: i64, seed: u32) -> f64 {
    let a = (ix as i64 as u32).wrapping_mul(0x27D4_EB2D);
    let b = (iy as i64 as u32).wrapping_mul(0x1656_67B1);
    (hash_u32(a ^ b ^ seed) as f64) / (u32::MAX as f64)
}

/// Smoothstep-interpolated 2D value noise. Output in `[0, 1]`. World
/// coordinates as input — the same `(x, y)` always returns the same
/// value, regardless of which tile it was sampled from.
fn value_noise_2d(x: f64, y: f64, seed: u32) -> f64 {
    let xi = x.floor();
    let yi = y.floor();
    let xf = x - xi;
    let yf = y - yi;
    let ix = xi as i64;
    let iy = yi as i64;
    let v00 = hash2(ix, iy, seed);
    let v10 = hash2(ix + 1, iy, seed);
    let v01 = hash2(ix, iy + 1, seed);
    let v11 = hash2(ix + 1, iy + 1, seed);
    let sx = xf * xf * (3.0 - 2.0 * xf);
    let sy = yf * yf * (3.0 - 2.0 * yf);
    let a = v00 * (1.0 - sx) + v10 * sx;
    let b = v01 * (1.0 - sx) + v11 * sx;
    a * (1.0 - sy) + b * sy
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
        let noise_amp_px = read_number_or(fields, "noise-amp-px", ctx, 0.0)?;
        // Default noise scale to wavelength when not provided.
        let noise_scale_px =
            read_number_or(fields, "noise-scale-px", ctx, wavelength_px.max(1.0))?;
        let seed = match fields.get("seed") {
            Some(Value::Number(n)) => n.as_u64().map(|v| v as u32),
            _ => None,
        };
        Ok(BuiltNode {
            node: Box::new(WaveNode {
                amplitude_px,
                wavelength_px,
                phase_px,
                samples_per_wavelength,
                noise_amp_px,
                noise_scale_px,
                seed,
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
                "noise-amp-px": { "type": "number",
                                  "description": "Peak 1D-value-noise jitter added on top of the sine, in pixels. 0 = pure sine (default)." },
                "noise-scale-px": { "type": "number", "minimum": 0.5,
                                    "description": "Cell length of the noise jitter along arc length, in pixels. Defaults to `wavelength-px`." },
                "seed": { "type": "integer", "minimum": 0,
                          "description": "Optional explicit u32 seed for the world-anchored 2D value noise. Default: a fixed constant so adjacent tiles agree across the seam." },
            },
            "required": ["features", "amplitude-px", "wavelength-px"],
        })
    }
}

ezu_graph::submit_node!(WaveFactory);
