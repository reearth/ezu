//! Deterministic seeding from world coordinates.
//!
//! The same world position with the same salt always produces the same seed,
//! regardless of which tile is being rendered. This is the foundation of
//! seamless tile boundaries: any jitter, scatter, or noise derived from this
//! seed will be continuous across tile edges.

use xxhash_rust::xxh3::xxh3_64_with_seed;

use crate::coord::WorldPos;

/// Produce a deterministic 64-bit seed from a world position and a salt.
///
/// The `salt` lets different consumers (e.g., dab jitter vs. paper noise)
/// derive uncorrelated sequences from the same position.
#[inline]
pub fn world_seed(pos: WorldPos, salt: u32) -> u64 {
    let mut bytes = [0u8; 16];
    bytes[0..8].copy_from_slice(&pos.x.to_bits().to_le_bytes());
    bytes[8..16].copy_from_slice(&pos.y.to_bits().to_le_bytes());
    xxh3_64_with_seed(&bytes, salt as u64)
}

/// Produce a deterministic 64-bit seed from a pair of integer lattice
/// indices and a salt.
///
/// Same role as [`world_seed`] for consumers that quantize the world into
/// cells before seeding: integer indices are exact, so two tiles covering
/// the same cell always agree, with no dependence on which float world
/// position was used to find it.
#[inline]
pub fn cell_seed(i: i64, j: i64, salt: u32) -> u64 {
    let mut bytes = [0u8; 16];
    bytes[0..8].copy_from_slice(&i.to_le_bytes());
    bytes[8..16].copy_from_slice(&j.to_le_bytes());
    xxh3_64_with_seed(&bytes, salt as u64)
}

/// Advance an LCG state and return the next value in `[0, 1)`.
///
/// Seeded from [`world_seed`] or [`cell_seed`], this is the jitter source
/// behind seamless scatter: the sequence depends only on the seed, never
/// on which tile is being rendered or in what order.
#[inline]
pub fn next_unit(state: &mut u64) -> f32 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    // Take the top 32 bits — the low bits of an LCG are the weak ones —
    // and normalize by 2^32 so the result spans the whole unit interval.
    let x = (*state >> 32) as u32;
    (x as f32) * (1.0 / (1u64 << 32) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_position_same_seed() {
        let a = world_seed(WorldPos::new(0.123, 0.456), 1);
        let b = world_seed(WorldPos::new(0.123, 0.456), 1);
        assert_eq!(a, b);
    }

    #[test]
    fn different_salt_different_seed() {
        let pos = WorldPos::new(0.5, 0.5);
        assert_ne!(world_seed(pos, 1), world_seed(pos, 2));
    }

    #[test]
    fn cell_seed_is_stable_and_distinct() {
        assert_eq!(cell_seed(-7, 12, 3), cell_seed(-7, 12, 3));
        assert_ne!(cell_seed(-7, 12, 3), cell_seed(12, -7, 3));
        assert_ne!(cell_seed(-7, 12, 3), cell_seed(-7, 12, 4));
    }

    #[test]
    fn next_unit_stays_in_range() {
        let mut state = cell_seed(1, 2, 0);
        for _ in 0..1000 {
            let u = next_unit(&mut state);
            assert!((0.0..1.0).contains(&u), "u = {u}");
        }
    }

    /// Consumers build symmetric jitter as `(u - 0.5) * 2 * amount` and
    /// acceptance tests as `u < p`, so the draws have to fill the unit
    /// interval, not a sub-range of it.
    #[test]
    fn next_unit_spans_the_unit_interval() {
        let mut state = cell_seed(7, 9, 1);
        let n = 20_000;
        let mut sum = 0.0f64;
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for _ in 0..n {
            let u = next_unit(&mut state);
            sum += u as f64;
            lo = lo.min(u);
            hi = hi.max(u);
        }
        assert!(lo < 0.01, "never drew near 0 (min {lo})");
        assert!(hi > 0.99, "never drew near 1 (max {hi})");
        let mean = sum / n as f64;
        assert!((mean - 0.5).abs() < 0.02, "mean {mean}");
    }
}
