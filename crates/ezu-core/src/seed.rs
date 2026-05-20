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
}
