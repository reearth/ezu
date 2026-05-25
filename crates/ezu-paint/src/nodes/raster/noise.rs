//! `noise` — `() -> Raster`. Procedural noise raster source.
//!
//! A single op covering several noise flavours, all driven by the same
//! shared parameters:
//!
//! - `type`: `white` | `value` | `perlin` | `simplex` | `worley`
//! - `scale-px`: wavelength in pixels at the current zoom (required)
//! - `octaves` / `lacunarity` / `gain`: fBm stack (set `octaves: 1` to
//!   disable)
//! - `warp-amp` / `warp-freq`: optional domain warp (turbulence)
//! - `low-color` / `high-color` / `opacity`: map the normalized noise
//!   value to RGBA
//! - `anchor`: `world` (default, seamless across tile borders) or
//!   `tile` (per-tile pattern)
//! - `seed`: explicit `u32`, otherwise derived from `EvalCtx::rng_seed`

use std::sync::Arc;

use ezu_graph::{
    schema_frag, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, Node,
    NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{read_color, read_number, read_number_or, read_optional_string, Anchor};
use crate::nodes::raster::noise_field::{fbm, NoiseKind, Sampler};

struct NoiseNode {
    kind: NoiseKind,
    scale_px: f64,
    octaves: u32,
    lacunarity: f64,
    gain: f64,
    warp_amp: f64,
    warp_freq: f64,
    seed: Option<u32>,
    low: [f32; 4],
    high: [f32; 4],
    opacity: f32,
    anchor: Anchor,
}

impl Node for NoiseNode {
    fn op_name(&self) -> &'static str {
        "noise"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn coord_space(&self) -> CoordSpace {
        match self.anchor {
            Anchor::World => CoordSpace::World,
            Anchor::Tile => CoordSpace::Tile,
        }
    }
    fn eval(&self, ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        let size = ctx.canvas.padded_size();
        let pad = ctx.canvas.pad as f64;
        let tile_size = ctx.canvas.tile_size as f64;
        let mut out = RasterBuf::new(size, size);

        let seed = self
            .seed
            .unwrap_or((ctx.rng_seed as u32) ^ ((ctx.rng_seed >> 32) as u32));
        let main = Sampler::build(self.kind, seed);
        // Warp uses an offset seed so the warp field decorrelates from
        // the main field. Only built when warp is active.
        let warp = if self.warp_amp != 0.0 {
            Some((
                Sampler::build(self.kind, seed.wrapping_add(0x9E37_79B9)),
                Sampler::build(self.kind, seed.wrapping_add(0x1234_5678)),
            ))
        } else {
            None
        };

        // World-origin of this tile in pixels at the current zoom.
        let (origin_x, origin_y) = match self.anchor {
            Anchor::World => (ctx.tile.x as f64 * tile_size, ctx.tile.y as f64 * tile_size),
            Anchor::Tile => (0.0, 0.0),
        };

        let inv_scale = 1.0 / self.scale_px;
        let warp_inv_scale = inv_scale * self.warp_freq;

        let [lr, lg, lb, la] = self.low;
        let [hr, hg, hb, ha] = self.high;
        let opacity = self.opacity.clamp(0.0, 1.0);

        for y in 0..size {
            let py = origin_y + (y as f64) - pad;
            for x in 0..size {
                let px = origin_x + (x as f64) - pad;

                let (sx, sy) = if let Some((wa, wb)) = warp.as_ref() {
                    let wx = wa.sample(px * warp_inv_scale, py * warp_inv_scale);
                    let wy = wb.sample(px * warp_inv_scale, py * warp_inv_scale);
                    (
                        (px + wx * self.warp_amp) * inv_scale,
                        (py + wy * self.warp_amp) * inv_scale,
                    )
                } else {
                    (px * inv_scale, py * inv_scale)
                };

                let n = fbm(&main, sx, sy, self.octaves, self.lacunarity, self.gain);
                let t = ((n * 0.5) + 0.5).clamp(0.0, 1.0) as f32;

                let r = lr + (hr - lr) * t;
                let g = lg + (hg - lg) * t;
                let b = lb + (hb - lb) * t;
                let a = (la + (ha - la) * t) * opacity;
                let i = ((y * size + x) * 4) as usize;
                out.pixels[i] = (r * a * 255.0).round() as u8;
                out.pixels[i + 1] = (g * a * 255.0).round() as u8;
                out.pixels[i + 2] = (b * a * 255.0).round() as u8;
                out.pixels[i + 3] = (a * 255.0).round() as u8;
            }
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"noise");
        h.update(&[self.kind.tag()]);
        h.update(&self.scale_px.to_le_bytes());
        h.update(&self.octaves.to_le_bytes());
        h.update(&self.lacunarity.to_le_bytes());
        h.update(&self.gain.to_le_bytes());
        h.update(&self.warp_amp.to_le_bytes());
        h.update(&self.warp_freq.to_le_bytes());
        match self.seed {
            Some(s) => {
                h.update(&[1]);
                h.update(&s.to_le_bytes());
            }
            None => h.update(&[0]),
        }
        for c in self.low {
            h.update(&c.to_le_bytes());
        }
        for c in self.high {
            h.update(&c.to_le_bytes());
        }
        h.update(&self.opacity.to_le_bytes());
        h.update(match self.anchor {
            Anchor::Tile => &[0u8],
            Anchor::World => &[1u8],
        });
    }
}

pub(super) struct NoiseFactory;
impl NodeFactory for NoiseFactory {
    fn op_name(&self) -> &'static str {
        "noise"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let kind = match read_optional_string(fields, "type")?.as_deref() {
            None => NoiseKind::Perlin,
            Some(s) => NoiseKind::parse(s).ok_or_else(|| FactoryError::BadField {
                field: "type".into(),
                msg: format!(
                    "unknown noise type `{s}`, expected white/value/perlin/simplex/worley"
                ),
            })?,
        };

        let scale_px = read_number(fields, "scale-px", ctx)?;
        if scale_px <= 0.0 {
            return Err(FactoryError::BadField {
                field: "scale-px".into(),
                msg: "scale-px must be > 0".into(),
            });
        }
        let octaves = read_number_or(fields, "octaves", ctx, 1.0)? as u32;
        let octaves = octaves.clamp(1, 12);
        let lacunarity = read_number_or(fields, "lacunarity", ctx, 2.0)?;
        let gain = read_number_or(fields, "gain", ctx, 0.5)?;
        let warp_amp = read_number_or(fields, "warp-amp", ctx, 0.0)?;
        let warp_freq = read_number_or(fields, "warp-freq", ctx, 1.0)?;

        let seed = match fields.get("seed") {
            None => None,
            Some(v) if v.is_null() => None,
            Some(v) => Some(v.as_u64().ok_or_else(|| FactoryError::BadField {
                field: "seed".into(),
                msg: "expected non-negative integer".into(),
            })? as u32),
        };

        let low = if fields.contains_key("low-color") {
            read_color(fields, "low-color", ctx)?
        } else {
            [0.0, 0.0, 0.0, 1.0]
        };
        let high = if fields.contains_key("high-color") {
            read_color(fields, "high-color", ctx)?
        } else {
            [1.0, 1.0, 1.0, 1.0]
        };
        let opacity = read_number_or(fields, "opacity", ctx, 1.0)? as f32;

        let anchor = match read_optional_string(fields, "anchor")?.as_deref() {
            None | Some("world") => Anchor::World,
            Some("tile") => Anchor::Tile,
            Some(other) => {
                return Err(FactoryError::BadField {
                    field: "anchor".into(),
                    msg: format!("unknown anchor `{other}`, expected tile/world"),
                });
            }
        };

        Ok(BuiltNode {
            node: Box::new(NoiseNode {
                kind,
                scale_px,
                octaves,
                lacunarity,
                gain,
                warp_amp,
                warp_freq,
                seed,
                low,
                high,
                opacity,
                anchor,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Procedural noise raster source. `scale-px` is the wavelength in pixels at the current zoom; `octaves`>1 stacks fBm; `warp-amp`>0 enables domain warp. `anchor=world` (default) keeps the field seamless across tile borders.",
            "properties": {
                "type": {
                    "type": "string",
                    "enum": ["white", "value", "perlin", "simplex", "worley"],
                    "default": "perlin",
                },
                "scale-px": schema_frag::px_number(),
                "octaves": { "type": "integer", "minimum": 1, "maximum": 12, "default": 1 },
                "lacunarity": { "type": "number", "default": 2.0 },
                "gain": { "type": "number", "default": 0.5 },
                "warp-amp": { "type": "number", "default": 0.0 },
                "warp-freq": { "type": "number", "default": 1.0 },
                "seed": { "type": "integer", "minimum": 0 },
                "low-color": schema_frag::color(),
                "high-color": schema_frag::color(),
                "opacity": schema_frag::unit_number(),
                "anchor": { "type": "string", "enum": ["tile", "world"], "default": "world" },
            },
            "required": ["scale-px"],
        })
    }
}

ezu_graph::submit_node!(NoiseFactory);
