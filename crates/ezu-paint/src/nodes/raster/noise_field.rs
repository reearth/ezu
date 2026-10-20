//! Shared noise sampler used by `noise` and `warp`.
//!
//! Centralises the `NoiseKind` enum + `Sampler` dispatch + fBm
//! accumulator so both ops describe the same noise space.

use ezu_graph::{EnumTable, EvalCtx, EvalError, FactoryError, In, InReader, PortValue};
use noise::{core::worley::ReturnType, NoiseFn, Perlin, Simplex, Value as ValueNoise, Worley};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoiseKind {
    White,
    Value,
    Perlin,
    Simplex,
    Worley,
    Cell,
}

/// The names `type` accepts, in the order the schema lists them.
pub(super) const NOISE_TYPES: EnumTable<NoiseKind> = &[
    ("white", NoiseKind::White),
    ("value", NoiseKind::Value),
    ("perlin", NoiseKind::Perlin),
    ("simplex", NoiseKind::Simplex),
    ("worley", NoiseKind::Worley),
    ("cell", NoiseKind::Cell),
];

/// The schema of a noise `type` field.
pub(super) fn noise_type_schema() -> Value {
    ezu_graph::schema_frag::in_enum(serde_json::json!({
        "type": "string",
        "enum": ezu_graph::enum_names(NOISE_TYPES),
        "default": "perlin",
    }))
}

pub(super) enum Sampler {
    White(u32),
    Value(ValueNoise),
    Perlin(Perlin),
    Simplex(Simplex),
    /// Distance to the nearest Worley site.
    Worley(Worley),
    /// The nearest Worley site's own random value, flat across its cell.
    Cell(Worley),
}

impl Sampler {
    pub(super) fn build(kind: NoiseKind, seed: u32) -> Self {
        match kind {
            NoiseKind::White => Sampler::White(seed),
            NoiseKind::Value => Sampler::Value(ValueNoise::new(seed)),
            NoiseKind::Perlin => Sampler::Perlin(Perlin::new(seed)),
            NoiseKind::Simplex => Sampler::Simplex(Simplex::new(seed)),
            NoiseKind::Worley => {
                Sampler::Worley(Worley::new(seed).set_return_type(ReturnType::Distance))
            }
            NoiseKind::Cell => Sampler::Cell(Worley::new(seed).set_return_type(ReturnType::Value)),
        }
    }

    /// Single sample. Output is normalized to roughly `[-1, 1]` so fBm
    /// accumulation stays well-behaved regardless of kind.
    pub(super) fn sample(&self, x: f64, y: f64) -> f64 {
        match self {
            // `white` hashes the coordinate straight into xxh3 and never
            // touches a lattice, so it takes the world coordinate as is.
            Sampler::White(seed) => white_hash(x, y, *seed) * 2.0 - 1.0,
            Sampler::Value(n) => n.get([fold(x), fold(y)]).clamp(-1.0, 1.0),
            Sampler::Perlin(n) => n.get([fold(x), fold(y)]).clamp(-1.0, 1.0),
            Sampler::Simplex(n) => n.get([fold(x), fold(y)]).clamp(-1.0, 1.0),
            // The crate returns `2·distance − 1`, distance in cell units;
            // negate it so a site is bright (+1) and the field falls
            // towards the cell edges: `1 − 2·distance`.
            Sampler::Worley(n) => (-n.get([fold(x), fold(y)])).clamp(-1.0, 1.0),
            // Already spread evenly over `[-1, 1]`, one value per cell.
            Sampler::Cell(n) => n.get([fold(x), fold(y)]).clamp(-1.0, 1.0),
        }
    }
}

/// The lattice kinds floor their input into an `isize` before hashing it.
/// `isize` is 32 bits wide on `wasm32`, and the cast is an `unwrap` on
/// `NumCast::from`, so a coordinate past `i32` range aborts the render
/// instead of producing a value. A world-anchored field grows its
/// coordinate with zoom — `tile · tile_size / scale-px · lacunarity^n` —
/// so deep tiles reach that range on the browser build while the native
/// build, with its 64-bit `isize`, keeps going.
///
/// Every lattice kind hashes its cell coordinate under `& 0xff`, so the
/// field repeats exactly every 256 units along each axis. Folding a
/// coordinate back into that period therefore returns the same value it
/// would have had, and bounds what reaches the cast. `simplex` skews the
/// lattice before flooring, so 256 is not one of its periods: it is
/// unaffected below the bound and gains a seam on the fold lines past it.
const LATTICE_PERIOD: f64 = 256.0;

/// Coordinates stay untouched below this magnitude, which keeps every
/// zoom that renders today byte-identical. The bound leaves room for
/// `simplex`, whose skew inflates the coordinate by about 1.4x before it
/// is floored.
const FOLD_ABOVE: f64 = (1u64 << 28) as f64;

/// Fold a coordinate into the lattice period once it grows past the point
/// where flooring it into a 32-bit `isize` would fail.
fn fold(v: f64) -> f64 {
    if v.abs() < FOLD_ABOVE {
        v
    } else if v.is_finite() {
        v.rem_euclid(LATTICE_PERIOD)
    } else {
        // A non-finite coordinate has no cell at all, and flooring one
        // fails the same way an out-of-range coordinate does.
        0.0
    }
}

/// Deterministic per-coordinate hash in `[0, 1)`. Used for `white`
/// noise so each global pixel gets a distinct value.
pub(super) fn white_hash(x: f64, y: f64, seed: u32) -> f64 {
    let mut h = Xxh3::new();
    h.update(&seed.to_le_bytes());
    h.update(&x.to_bits().to_le_bytes());
    h.update(&y.to_bits().to_le_bytes());
    let v = h.digest();
    ((v >> 11) as f64) * (1.0 / ((1u64 << 53) as f64))
}

pub(super) fn fbm(
    sampler: &Sampler,
    x: f64,
    y: f64,
    octaves: u32,
    lacunarity: f64,
    gain: f64,
) -> f64 {
    if octaves <= 1 {
        return sampler.sample(x, y);
    }
    let mut sum = 0.0;
    let mut amp = 1.0;
    let mut freq = 1.0;
    let mut norm = 0.0;
    for _ in 0..octaves {
        sum += sampler.sample(x * freq, y * freq) * amp;
        norm += amp;
        amp *= gain;
        freq *= lacunarity;
    }
    if norm > 0.0 {
        sum / norm
    } else {
        0.0
    }
}

/// A noise wavelength, `scale-px`, as `noise` and `warp` both read it: one
/// value for both axes, or `[x, y]` to stretch the pattern along one.
pub(super) struct ScalePx {
    pub(super) x: In<f64>,
    pub(super) y: In<f64>,
    /// Whether the field was written as `[x, y]`. A single value fills both
    /// axes, and a node that hashed one value before keeps hashing one.
    pub(super) per_axis: bool,
}

impl ScalePx {
    /// Read `scale-px`. A single value may be a literal, a `$param` or an
    /// `@node`. Each entry of an `[x, y]` pair may be a literal or a
    /// `$param`, but not an `@node`: a scalar port is wired to a whole
    /// field, not to one entry inside it. Each axis must be > 0 wherever
    /// that is known at build time — a literal, or a `$param`'s `max`.
    pub(super) fn read(
        r: &mut InReader<'_, '_>,
        fields: &serde_json::Map<String, Value>,
    ) -> Result<Self, FactoryError> {
        const NAME: &str = "scale-px";
        let scale = match fields.get(NAME).and_then(Value::as_array) {
            Some(arr) => {
                if arr.len() != 2 {
                    return Err(FactoryError::BadField {
                        field: NAME.into(),
                        msg: format!(
                            "expected number or [x, y], got array of length {}",
                            arr.len()
                        ),
                    });
                }
                ScalePx {
                    x: r.nested("scale-px[0]", &arr[0])?,
                    y: r.nested("scale-px[1]", &arr[1])?,
                    per_axis: true,
                }
            }
            None => {
                let v = r.number(NAME)?;
                ScalePx {
                    x: v.clone(),
                    y: v,
                    per_axis: false,
                }
            }
        };
        for axis in [&scale.x, &scale.y] {
            if axis.static_bound().is_some_and(|b| b <= 0.0) {
                return Err(FactoryError::BadField {
                    field: NAME.into(),
                    msg: "scale-px must be > 0".into(),
                });
            }
        }
        Ok(scale)
    }

    /// The reciprocal wavelengths `(1 / x, 1 / y)` for one eval. An axis
    /// whose value is not positive — only possible from a port or an
    /// unbounded `$param` — gets 0, a field that does not vary along it,
    /// rather than an infinite frequency.
    pub(super) fn inv(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<(f64, f64), EvalError> {
        let inv = |v: f64| if v > 0.0 { 1.0 / v } else { 0.0 };
        Ok((inv(self.x.get(ctx, inputs)?), inv(self.y.get(ctx, inputs)?)))
    }

    /// The field's schema, shared by both ops.
    pub(super) fn schema() -> Value {
        serde_json::json!({
            "description": "Noise wavelength in pixels. A single number for isotropic noise; an `[x, y]` array for anisotropic noise (larger value = longer wavelength along that axis, i.e. stretched pattern). A single value may be a `$param` or an `@node`; each entry of the array may be a number or a `$param`.",
            "oneOf": [
                { "type": "number", "exclusiveMinimum": 0 },
                {
                    "type": "string",
                    "pattern": "^[$@][A-Za-z_][A-Za-z0-9_-]*$",
                    "description": "`$param` reference or `@node` scalar port."
                },
                { "type": "array",
                  "items": ezu_graph::schema_frag::nested_number(
                      serde_json::json!({ "type": "number", "exclusiveMinimum": 0 })
                  ),
                  "minItems": 2, "maxItems": 2 },
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole fold rests on the lattice hash masking its cell
    /// coordinate with `0xff`. Pin that period down. `worley` measures a
    /// distance from the shifted coordinate, so it repeats to within
    /// rounding rather than bit for bit.
    #[test]
    fn lattice_kinds_repeat_every_256_units() {
        for kind in [
            NoiseKind::Value,
            NoiseKind::Perlin,
            NoiseKind::Worley,
            NoiseKind::Cell,
        ] {
            let s = Sampler::build(kind, 7);
            for (x, y) in [(0.25, 0.75), (13.5, -4.25), (-100.125, 60.0)] {
                let a = s.sample(x, y);
                let b = s.sample(x + LATTICE_PERIOD * 3.0, y - LATTICE_PERIOD * 5.0);
                assert!((a - b).abs() < 1e-9, "{kind:?} at ({x}, {y}): {a} vs {b}");
            }
        }
    }

    #[test]
    fn coordinates_below_the_bound_are_untouched() {
        for v in [0.0, -1.5, 1e6, FOLD_ABOVE - 1.0, -(FOLD_ABOVE - 1.0)] {
            assert_eq!(fold(v), v);
        }
    }

    /// Whatever the coordinate, what reaches the lattice has to survive
    /// being floored into a 32-bit `isize`.
    #[test]
    fn folded_coordinates_fit_a_32_bit_isize() {
        // A `simplex` skew inflates the coordinate before it is floored,
        // so leave that headroom in the bound being asserted.
        let ceiling = i32::MAX as f64 / 2.0;
        for v in [
            FOLD_ABOVE,
            -FOLD_ABOVE,
            2.4e9,
            -2.4e9,
            f64::MAX,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
        ] {
            let folded = fold(v);
            assert!(
                folded.is_finite() && folded.abs() < ceiling,
                "fold({v}) = {folded}",
            );
        }
    }

    /// The z20 tile of `pencil-sketch` reaches roughly 2.4e9 in the
    /// deepest octave, which used to abort the browser render.
    #[test]
    fn deep_world_coordinates_sample_instead_of_aborting() {
        for kind in [
            NoiseKind::White,
            NoiseKind::Value,
            NoiseKind::Perlin,
            NoiseKind::Simplex,
            NoiseKind::Worley,
            NoiseKind::Cell,
        ] {
            let s = Sampler::build(kind, 1);
            let v = fbm(&s, 2.4e9, -2.4e9, 4, 2.1, 0.5);
            assert!(v.is_finite() && (-1.0..=1.0).contains(&v), "{kind:?}: {v}");
        }
    }

    /// Folding keeps the field itself intact for the kinds whose lattice
    /// is axis-aligned, so a deep tile still matches the field a shallow
    /// one draws.
    #[test]
    fn folding_preserves_the_field_for_axis_aligned_lattices() {
        for kind in [
            NoiseKind::Value,
            NoiseKind::Perlin,
            NoiseKind::Worley,
            NoiseKind::Cell,
        ] {
            let s = Sampler::build(kind, 3);
            let far = FOLD_ABOVE * 16.0;
            assert_eq!(
                s.sample(far + 0.5, far + 0.25),
                s.sample(0.5, 0.25),
                "{kind:?}"
            );
        }
    }

    /// Sample a 64 × 64 grid of points spread over several cells.
    fn samples(kind: NoiseKind) -> Vec<f64> {
        let s = Sampler::build(kind, 5);
        (0..64 * 64)
            .map(|i| s.sample((i % 64) as f64 * 0.13, (i / 64) as f64 * 0.13))
            .collect()
    }

    /// `worley` is a distance: bright at each site, falling away from it,
    /// and continuous — never a plateau pinned at +1 over whole cells.
    #[test]
    fn worley_is_a_distance_field() {
        let v = samples(NoiseKind::Worley);
        let pinned = v.iter().filter(|&&x| x >= 1.0).count();
        assert!(
            pinned < v.len() / 100,
            "{pinned} of {} samples at +1",
            v.len()
        );
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        assert!((-0.5..0.8).contains(&mean), "mean {mean}");
    }

    /// `cell` is one value per cell, spread over the whole range rather
    /// than clamped into half of it.
    #[test]
    fn cell_values_span_the_range() {
        let v = samples(NoiseKind::Cell);
        assert!(v.iter().any(|&x| x < -0.5), "no low cells");
        assert!(v.iter().any(|&x| x > 0.5), "no high cells");
        let mut distinct: Vec<f64> = v.clone();
        distinct.sort_by(f64::total_cmp);
        distinct.dedup();
        // ~70 cells under the grid, each flat: far fewer values than samples.
        assert!(
            distinct.len() < v.len() / 20,
            "{} distinct values",
            distinct.len()
        );
    }
}
