//! Shared noise sampler used by `noise` and `warp`.
//!
//! Centralises the `NoiseKind` enum + `Sampler` dispatch + fBm
//! accumulator so both ops describe the same noise space.

use noise::{NoiseFn, Perlin, Simplex, Value as ValueNoise, Worley};
use xxhash_rust::xxh3::Xxh3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoiseKind {
    White,
    Value,
    Perlin,
    Simplex,
    Worley,
}

impl NoiseKind {
    pub(super) fn tag(self) -> u8 {
        match self {
            NoiseKind::White => 0,
            NoiseKind::Value => 1,
            NoiseKind::Perlin => 2,
            NoiseKind::Simplex => 3,
            NoiseKind::Worley => 4,
        }
    }

    pub(super) fn parse(s: &str) -> Option<NoiseKind> {
        Some(match s {
            "white" => NoiseKind::White,
            "value" => NoiseKind::Value,
            "perlin" => NoiseKind::Perlin,
            "simplex" => NoiseKind::Simplex,
            "worley" => NoiseKind::Worley,
            _ => return None,
        })
    }
}

pub(super) enum Sampler {
    White(u32),
    Value(ValueNoise),
    Perlin(Perlin),
    Simplex(Simplex),
    Worley(Worley),
}

impl Sampler {
    pub(super) fn build(kind: NoiseKind, seed: u32) -> Self {
        match kind {
            NoiseKind::White => Sampler::White(seed),
            NoiseKind::Value => Sampler::Value(ValueNoise::new(seed)),
            NoiseKind::Perlin => Sampler::Perlin(Perlin::new(seed)),
            NoiseKind::Simplex => Sampler::Simplex(Simplex::new(seed)),
            NoiseKind::Worley => Sampler::Worley(Worley::new(seed)),
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
            Sampler::Worley(n) => 1.0 - 2.0 * n.get([fold(x), fold(y)]).clamp(0.0, 1.0),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole fold rests on the lattice hash masking its cell
    /// coordinate with `0xff`. Pin that period down.
    #[test]
    fn lattice_kinds_repeat_every_256_units() {
        for kind in [NoiseKind::Value, NoiseKind::Perlin, NoiseKind::Worley] {
            let s = Sampler::build(kind, 7);
            for (x, y) in [(0.25, 0.75), (13.5, -4.25), (-100.125, 60.0)] {
                assert_eq!(
                    s.sample(x, y),
                    s.sample(x + LATTICE_PERIOD * 3.0, y - LATTICE_PERIOD * 5.0),
                    "{kind:?} at ({x}, {y})",
                );
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
        for kind in [NoiseKind::Value, NoiseKind::Perlin, NoiseKind::Worley] {
            let s = Sampler::build(kind, 3);
            let far = FOLD_ABOVE * 16.0;
            assert_eq!(
                s.sample(far + 0.5, far + 0.25),
                s.sample(0.5, 0.25),
                "{kind:?}"
            );
        }
    }
}
