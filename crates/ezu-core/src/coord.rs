//! Web-Mercator tile / world coordinate utilities.
//!
//! World coordinates are in the unit square `[0, 1] x [0, 1]` covering the whole
//! Web-Mercator projection. This avoids zoom-dependent units and makes
//! deterministic seeding zoom-stable.

/// A Web-Mercator XYZ tile identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileId {
    pub const fn new(z: u8, x: u32, y: u32) -> Self {
        Self { z, x, y }
    }

    /// Number of tiles along one axis at this zoom level.
    #[inline]
    pub fn axis_tiles(self) -> u32 {
        1u32 << self.z
    }
}

/// A position in the global Web-Mercator unit square `[0, 1] x [0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldPos {
    pub x: f64,
    pub y: f64,
}

impl WorldPos {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// Convert a tile-local position (in `[0, extent]`) to world unit-square coordinates.
#[inline]
pub fn tile_to_world(tile: TileId, tx: f64, ty: f64, extent: f64) -> WorldPos {
    let n = tile.axis_tiles() as f64;
    WorldPos {
        x: (tile.x as f64 + tx / extent) / n,
        y: (tile.y as f64 + ty / extent) / n,
    }
}
