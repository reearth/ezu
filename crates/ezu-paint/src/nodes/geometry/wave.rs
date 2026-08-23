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
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, InfluenceCtx, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, read_number_or, FeatureGroup};

struct WaveNode {
    amplitude_px: In<f64>,
    wavelength_px: In<f64>,
    phase_px: In<f64>,
    samples_per_wavelength: u32,
    noise_amp_px: In<f64>,
    /// Cell size of the noise jitter. A non-positive value (the
    /// default sentinel) means "fall back to `wavelength-px`".
    noise_scale_px: In<f64>,
    seed: Option<u32>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for WaveNode {
    fn op_name(&self) -> &'static str {
        "wave"
    }

    /// A wave moves a vertex by at most the sine amplitude plus the
    /// noise amplitude, so geometry that far outside the canvas can
    /// still be displaced onto it.
    fn influence_pad(&self, ctx: &InfluenceCtx<'_>) -> u32 {
        let (Some(amp), Some(noise)) = (
            self.amplitude_px.static_bound(),
            self.noise_amp_px.static_bound(),
        ) else {
            return InfluenceCtx::UNBOUNDED;
        };
        ctx.plus(amp.abs() + noise.abs())
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
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
        let amplitude_px = self.amplitude_px.get(ctx, inputs)?;
        let wavelength_px = self.wavelength_px.get(ctx, inputs)?;
        let phase_px = self.phase_px.get(ctx, inputs)?;
        let noise_amp_px = self.noise_amp_px.get(ctx, inputs)?;
        // A non-positive `noise-scale-px` means "use the wavelength".
        let noise_scale_raw = self.noise_scale_px.get(ctx, inputs)?;
        let noise_scale_px = if noise_scale_raw > 0.0 {
            noise_scale_raw
        } else {
            wavelength_px.max(1.0)
        };
        let scale = feats.extent as f64 / ctx.canvas.tile_w.max(1) as f64;
        let amp = amplitude_px * scale;
        let wavelen = wavelength_px * scale;
        let phase = phase_px * scale;
        let noise_amp = noise_amp_px * scale;
        let noise_scale = noise_scale_px * scale;
        // Use a constant default seed so the world-anchored noise is
        // deterministic across tiles. Per-tile rng_seed would make the
        // noise pattern shift at every tile border.
        let seed = self.seed.unwrap_or(0xA17F_B91D);
        // World origin of this tile in feature-extent units, so the
        // noise field is the same global function across tiles.
        let extent = feats.extent as f64;
        let origin_x = ctx.tile.x as f64 * extent;
        let origin_y = ctx.tile.y as f64 * extent;

        // Per group: displace each feature's polylines (polygons and points
        // pass through), carrying properties.
        let mut out_groups = Vec::with_capacity(feats.groups.len());
        for g in &feats.groups {
            let mut out_lines: Vec<Vec<(i32, i32)>> = Vec::with_capacity(g.lines.len());
            for line in g.lines.iter() {
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
            out_groups.push(FeatureGroup {
                properties: g.properties.clone(),
                polygons: g.polygons.clone(),
                lines: out_lines,
                points: g.points.clone(),
            });
        }
        Ok(features_value(feats.extent, out_groups))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"wave");
        self.amplitude_px.param_hash(h);
        self.wavelength_px.param_hash(h);
        self.phase_px.param_hash(h);
        h.update(&self.samples_per_wavelength.to_le_bytes());
        self.noise_amp_px.param_hash(h);
        self.noise_scale_px.param_hash(h);
        if let Some(s) = self.seed {
            h.update(b"seed");
            h.update(&s.to_le_bytes());
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
    let a = (ix as u32).wrapping_mul(0x27D4_EB2D);
    let b = (iy as u32).wrapping_mul(0x1656_67B1);
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
        // `samples-per-wavelength` is clamped to an integer count at
        // build time, so it stays a static literal.
        let samples_per_wavelength =
            read_number_or(fields, "samples-per-wavelength", ctx, 16.0)?.clamp(2.0, 256.0) as u32;
        let seed = match fields.get("seed") {
            Some(Value::Number(n)) => n.as_u64().map(|v| v as u32),
            _ => None,
        };

        let mut r = InReader::new(fields, ctx, 1);
        let amplitude_px = r.number("amplitude-px")?;
        let wavelength_px = r.number("wavelength-px")?;
        let phase_px = r.number_or("phase-px", 0.0)?;
        let noise_amp_px = r.number_or("noise-amp-px", 0.0)?;
        // Default noise scale to wavelength when not provided: use a
        // non-positive sentinel resolved at eval time (the build-time
        // wavelength value is no longer available as a literal).
        let noise_scale_px = r.number_or("noise-scale-px", 0.0)?;
        let parts = r.finish();

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
            node: Box::new(WaveNode {
                amplitude_px,
                wavelength_px,
                phase_px,
                samples_per_wavelength,
                noise_amp_px,
                noise_scale_px,
                seed,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Displace polylines laterally with a sine wave. Lengths in canvas pixels. Sharp corners may kink (no tangent smoothing). Polygons/points pass through.",
            "properties": {
                "features": schema_frag::node_ref(),
                "amplitude-px": schema_frag::in_number(serde_json::json!({ "type": "number",
                                  "description": "Peak lateral deviation in pixels. May be negative to flip phase." })),
                "wavelength-px": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.5 })),
                "phase-px": schema_frag::in_number(serde_json::json!({ "type": "number",
                              "description": "Offset into the wave at the polyline start, in pixels." })),
                "samples-per-wavelength": { "type": "number", "minimum": 2, "maximum": 256,
                                            "description": "Subdivisions per wavelength. Higher = smoother but more vertices. Default 16." },
                "noise-amp-px": schema_frag::in_number(serde_json::json!({ "type": "number",
                                  "description": "Peak 1D-value-noise jitter added on top of the sine, in pixels. 0 = pure sine (default)." })),
                "noise-scale-px": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.5,
                                    "description": "Cell length of the noise jitter along arc length, in pixels. Defaults to `wavelength-px`." })),
                "seed": { "type": "integer", "minimum": 0,
                          "description": "Optional explicit u32 seed for the world-anchored 2D value noise. Default: a fixed constant so adjacent tiles agree across the seam." },
            },
            "required": ["features", "amplitude-px", "wavelength-px"],
        })
    }
}

ezu_graph::submit_node!(WaveFactory);
