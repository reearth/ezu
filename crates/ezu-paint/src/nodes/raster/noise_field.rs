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
            Sampler::White(seed) => white_hash(x, y, *seed) * 2.0 - 1.0,
            Sampler::Value(n) => n.get([x, y]).clamp(-1.0, 1.0),
            Sampler::Perlin(n) => n.get([x, y]).clamp(-1.0, 1.0),
            Sampler::Simplex(n) => n.get([x, y]).clamp(-1.0, 1.0),
            Sampler::Worley(n) => 1.0 - 2.0 * n.get([x, y]).clamp(0.0, 1.0),
        }
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
