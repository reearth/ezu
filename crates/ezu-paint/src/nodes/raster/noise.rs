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
//! - `seed`: explicit `u32`, otherwise chosen by `anchor` (below)
//!
//! The default `seed` follows `anchor`: a world-anchored field takes a
//! fixed seed, because "seamless across tile borders" means every tile has
//! to sample the *same* field — the host's per-tile `rng_seed` would align
//! the sampling coordinates and then swap the field underneath them. A
//! tile-anchored field takes the per-tile seed, which is the variation it
//! exists for.
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
    schema_frag, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, In, InReader,
    Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf, ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    default_field_seed, read_number_or, read_optional_string, resolve_field, Anchor,
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
    lacunarity: In<f64>,
    gain: In<f64>,
    warp_amp: In<f64>,
    warp_freq: In<f64>,
    seed: Option<u32>,
    low: In<[f32; 4]>,
    high: In<[f32; 4]>,
    opacity: In<f64>,
    anchor: Anchor,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for NoiseNode {
    fn op_name(&self) -> &'static str {
        "noise"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
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
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let (pw, ph) = ctx.canvas.padded_dims();
        let pad = ctx.canvas.pad as f64;
        let tile_size = ctx.canvas.tile_w as f64;

        let lacunarity = self.lacunarity.get(ctx, inputs)?;
        let gain = self.gain.get(ctx, inputs)?;
        let warp_amp = self.warp_amp.get(ctx, inputs)?;
        let warp_freq = self.warp_freq.get(ctx, inputs)?;

        // Anchoring decides the default seed: a world-anchored field has to
        // be the same field in every tile, so it cannot use a per-tile one.
        let seed = self
            .seed
            .unwrap_or_else(|| default_field_seed(self.anchor, ctx.rng_seed));
        let main = Sampler::build(self.kind, seed);
        // Warp uses an offset seed so the warp field decorrelates from
        // the main field. Only built when warp is active.
        let warp = if warp_amp != 0.0 {
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
        let warp_inv_scale_x = inv_scale_x * warp_freq;
        let warp_inv_scale_y = inv_scale_y * warp_freq;

        // Sampling kernel shared between raster and scalar modes.
        let sample_at = |x: u32, y: u32| -> f64 {
            let py = origin_y + (y as f64) - pad;
            let px = origin_x + (x as f64) - pad;
            let (sx, sy) = if let Some((wa, wb)) = warp.as_ref() {
                let wx = wa.sample(px * warp_inv_scale_x, py * warp_inv_scale_y);
                let wy = wb.sample(px * warp_inv_scale_x, py * warp_inv_scale_y);
                (
                    (px + wx * warp_amp) * inv_scale_x,
                    (py + wy * warp_amp) * inv_scale_y,
                )
            } else {
                (px * inv_scale_x, py * inv_scale_y)
            };
            fbm(&main, sx, sy, self.octaves, lacunarity, gain)
        };

        match self.out_kind {
            OutputKind::Scalar => {
                let mut values: Vec<f32> = Vec::with_capacity((pw * ph) as usize);
                for y in 0..ph {
                    for x in 0..pw {
                        values.push(sample_at(x, y) as f32);
                    }
                }
                Ok(PortValue::ScalarField(Arc::new(ScalarField {
                    width: pw,
                    height: ph,
                    values: values.into(),
                    nodata: None,
                    geo_scale: None,
                })))
            }
            OutputKind::Raster => {
                let [lr, lg, lb, la] = self.low.get(ctx, inputs)?;
                let [hr, hg, hb, ha] = self.high.get(ctx, inputs)?;
                let opacity = (self.opacity.get(ctx, inputs)? as f32).clamp(0.0, 1.0);
                let mut out = RasterBuf::new(pw, ph);
                for y in 0..ph {
                    for x in 0..pw {
                        let n = sample_at(x, y);
                        let t = ((n * 0.5) + 0.5).clamp(0.0, 1.0) as f32;
                        let r = lr + (hr - lr) * t;
                        let g = lg + (hg - lg) * t;
                        let b = lb + (hb - lb) * t;
                        let a = (la + (ha - la) * t) * opacity;
                        let i = ((y * pw + x) * 4) as usize;
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
        self.lacunarity.param_hash(h);
        self.gain.param_hash(h);
        self.warp_amp.param_hash(h);
        self.warp_freq.param_hash(h);
        match self.seed {
            Some(s) => {
                h.update(&[1]);
                h.update(&s.to_le_bytes());
            }
            None => h.update(&[0]),
        }
        self.low.param_hash(h);
        self.high.param_hash(h);
        self.opacity.param_hash(h);
        h.update(match self.anchor {
            Anchor::Tile => &[0u8],
            Anchor::World => &[1u8],
        });
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        // `octaves` is clamped to an integer range at build time and
        // stored as a `u32`, so it stays a static field rather than an
        // `In<f64>` scalar input.
        let octaves = read_number_or(fields, "octaves", ctx, 1.0)? as u32;
        let octaves = octaves.clamp(1, 12);

        let seed = match fields.get("seed") {
            None => None,
            Some(v) if v.is_null() => None,
            Some(v) => Some(v.as_u64().ok_or_else(|| FactoryError::BadField {
                field: "seed".into(),
                msg: "expected non-negative integer".into(),
            })? as u32),
        };

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

        let mut r = InReader::new(fields, ctx, 0);
        let lacunarity = r.number_or("lacunarity", 2.0)?;
        let gain = r.number_or("gain", 0.5)?;
        let warp_amp = r.number_or("warp-amp", 0.0)?;
        let warp_freq = r.number_or("warp-freq", 1.0)?;
        let low = r.color_or("low-color", [0.0, 0.0, 0.0, 1.0])?;
        let high = r.color_or("high-color", [1.0, 1.0, 1.0, 1.0])?;
        let opacity = r.number_or("opacity", 1.0)?;
        let parts = r.finish();

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
                ports: parts.ports,
                param_refs: parts.param_refs,
            }),
            connections: parts.connections,
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
                "lacunarity": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 2.0 })),
                "gain": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.5 })),
                "warp-amp": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.0 })),
                "warp-freq": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 1.0 })),
                "seed": { "type": "integer", "minimum": 0, "description": "Field seed. Omitted, `anchor: world` uses a fixed seed so every tile samples the same field, and `anchor: tile` uses the host's per-tile seed so each tile gets its own. Set it to pin either." },
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
