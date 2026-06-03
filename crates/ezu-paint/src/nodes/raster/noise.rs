//! `noise` — procedural noise source. Emits a coloured `Raster`
//! (default) or a raw `ScalarField` depending on the `kind` field.
//!
//! Shared parameters:
//!
//! - `type`: `white` | `value` | `perlin` | `simplex` | `worley`
//! - `scale-px`: wavelength in pixels (required). Either a single
//!   number (isotropic) or `[x, y]` for anisotropic noise — useful for
//!   wood grain, brick patterns, or wave streaks.
//! - `octaves` / `lacunarity` / `gain`: fBm stack (set `octaves: 1` to
//!   disable)
//! - `warp-amp` / `warp-freq`: optional domain warp (turbulence)
//! - `anchor`: `world` (default, seamless across tile borders) or
//!   `tile` (per-tile pattern)
//! - `seed`: explicit `u32`, otherwise derived from `EvalCtx::rng_seed`
//!
//! Raster mode (default, `kind: "raster"`) also takes
//! `low-color` / `high-color` / `opacity` to map the normalised noise
//! value to RGBA.
//!
//! Scalar mode (`kind: "scalar"`) emits the **raw** fBm value as a
//! `ScalarField` (roughly `[-1, 1]` for value/perlin/simplex,
//! `[0, 1]`-ish for worley/white). Compose with `map-range` to
//! normalise before feeding `hillshade` / `slope` / `color-ramp`. The
//! field has no `geo_scale` — gradient consumers treat each pixel
//! as one unit, so the result is stylization-only, not geographically
//! faithful.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, Node,
    NodeFactory, PortKind, PortSpec, PortValue, RasterBuf, ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    read_color, read_number_or, read_optional_string, resolve_field, Anchor,
};
use crate::nodes::raster::noise_field::{fbm, NoiseKind, Sampler};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputKind {
    Raster,
    Scalar,
}

struct NoiseNode {
    kind: NoiseKind,
    out_kind: OutputKind,
    scale_x: f64,
    scale_y: f64,
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
        match self.out_kind {
            OutputKind::Raster => PortKind::Raster,
            OutputKind::Scalar => PortKind::ScalarField,
        }
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

        let (origin_x, origin_y) = match self.anchor {
            Anchor::World => (ctx.tile.x as f64 * tile_size, ctx.tile.y as f64 * tile_size),
            Anchor::Tile => (0.0, 0.0),
        };

        let inv_scale_x = 1.0 / self.scale_x;
        let inv_scale_y = 1.0 / self.scale_y;
        let warp_inv_scale_x = inv_scale_x * self.warp_freq;
        let warp_inv_scale_y = inv_scale_y * self.warp_freq;

        // Sampling kernel shared between raster and scalar modes.
        let sample_at = |x: u32, y: u32| -> f64 {
            let py = origin_y + (y as f64) - pad;
            let px = origin_x + (x as f64) - pad;
            let (sx, sy) = if let Some((wa, wb)) = warp.as_ref() {
                let wx = wa.sample(px * warp_inv_scale_x, py * warp_inv_scale_y);
                let wy = wb.sample(px * warp_inv_scale_x, py * warp_inv_scale_y);
                (
                    (px + wx * self.warp_amp) * inv_scale_x,
                    (py + wy * self.warp_amp) * inv_scale_y,
                )
            } else {
                (px * inv_scale_x, py * inv_scale_y)
            };
            fbm(&main, sx, sy, self.octaves, self.lacunarity, self.gain)
        };

        match self.out_kind {
            OutputKind::Scalar => {
                let mut values: Vec<f32> = Vec::with_capacity((size * size) as usize);
                for y in 0..size {
                    for x in 0..size {
                        values.push(sample_at(x, y) as f32);
                    }
                }
                Ok(PortValue::ScalarField(Arc::new(ScalarField {
                    width: size,
                    height: size,
                    values: values.into(),
                    nodata: None,
                    geo_scale: None,
                })))
            }
            OutputKind::Raster => {
                let [lr, lg, lb, la] = self.low;
                let [hr, hg, hb, ha] = self.high;
                let opacity = self.opacity.clamp(0.0, 1.0);
                let mut out = RasterBuf::new(size, size);
                for y in 0..size {
                    for x in 0..size {
                        let n = sample_at(x, y);
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
        }
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"noise");
        h.update(&[self.kind.tag()]);
        h.update(match self.out_kind {
            OutputKind::Raster => b"r",
            OutputKind::Scalar => b"s",
        });
        h.update(&self.scale_x.to_le_bytes());
        h.update(&self.scale_y.to_le_bytes());
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
        let out_kind = match read_optional_string(fields, "kind")?.as_deref() {
            None | Some("raster") => OutputKind::Raster,
            Some("scalar") | Some("scalar-field") => OutputKind::Scalar,
            Some(other) => {
                return Err(FactoryError::BadField {
                    field: "kind".into(),
                    msg: format!("expected `raster` or `scalar`, got `{other}`"),
                });
            }
        };

        let (scale_x, scale_y) = read_scale_xy(fields, "scale-px", ctx)?;
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
                out_kind,
                scale_x,
                scale_y,
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
            "description": "Procedural noise source. With `kind: raster` (default) the noise is mapped to RGBA via `low-color`/`high-color`/`opacity`. With `kind: scalar` it emits a `ScalarField` of raw fBm values (~[-1, 1] for value/perlin/simplex) — compose with `map-range` before feeding `hillshade`/`color-ramp`. `anchor=world` (default) keeps the field seamless across tile borders.",
            "properties": {
                "type": {
                    "type": "string",
                    "enum": ["white", "value", "perlin", "simplex", "worley"],
                    "default": "perlin",
                },
                "kind": {
                    "type": "string",
                    "enum": ["raster", "scalar"],
                    "default": "raster",
                },
                "scale-px": {
                    "description": "Noise wavelength in pixels. A single number for isotropic noise; an `[x, y]` array for anisotropic noise (larger value = longer wavelength along that axis, i.e. stretched pattern).",
                    "oneOf": [
                        { "type": "number", "exclusiveMinimum": 0 },
                        { "type": "array", "items": { "type": "number", "exclusiveMinimum": 0 },
                          "minItems": 2, "maxItems": 2 },
                    ],
                },
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

/// Read a field that may be either a single number (isotropic) or a
/// `[x, y]` array (anisotropic). Both axis values must be > 0.
fn read_scale_xy(
    fields: &serde_json::Map<String, Value>,
    name: &str,
    ctx: &FactoryCtx<'_>,
) -> Result<(f64, f64), FactoryError> {
    let v = resolve_field(fields, name, ctx)?;
    let (x, y) = if let Some(n) = v.as_f64() {
        (n, n)
    } else if let Some(arr) = v.as_array() {
        if arr.len() != 2 {
            return Err(FactoryError::BadField {
                field: name.into(),
                msg: format!(
                    "expected number or [x, y], got array of length {}",
                    arr.len()
                ),
            });
        }
        let x = arr[0].as_f64().ok_or_else(|| FactoryError::BadField {
            field: name.into(),
            msg: "x must be a number".into(),
        })?;
        let y = arr[1].as_f64().ok_or_else(|| FactoryError::BadField {
            field: name.into(),
            msg: "y must be a number".into(),
        })?;
        (x, y)
    } else {
        return Err(FactoryError::BadField {
            field: name.into(),
            msg: "expected number or [x, y] array".into(),
        });
    };
    if x <= 0.0 || y <= 0.0 {
        return Err(FactoryError::BadField {
            field: name.into(),
            msg: "scale components must be > 0".into(),
        });
    }
    Ok((x, y))
}
