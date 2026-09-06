//! Web-Mercator tile / world coordinate utilities.
//!
//! World coordinates are in the unit square `[0, 1] x [0, 1]` covering the whole
//! Web-Mercator projection. This avoids zoom-dependent units and makes
//! deterministic seeding zoom-stable.
//!
//! # Domain handling
//!
//! These are the raw projection formulas; **keeping inputs in range is the
//! caller's job**, and callers do clamp for their own needs. Only
//! [`lat_to_world_y`] clamps internally, because the projection diverges at
//! the poles and there is no meaningful value to return past
//! [`MERCATOR_MAX_LAT`]; that clamp is not invertible, so a latitude beyond
//! the domain does not survive a round trip. Everything else extrapolates:
//! [`lon_to_world_x`] maps `190` to `1.0277…`, and [`world_y_to_lat`] happily
//! reports latitudes past the pole for a world y outside `[0, 1]`.
//!
//! World coordinates outside `[0, 1]` arise in normal operation — a padded
//! canvas on the top row of tiles extends above `y = 0` — so consumers that
//! need a bounded value clamp at the call site.

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

    /// The tile one zoom level up that contains this tile, or `None`
    /// at zoom 0 (which has no parent).
    #[inline]
    pub fn parent(self) -> Option<TileId> {
        if self.z == 0 {
            None
        } else {
            Some(TileId::new(self.z - 1, self.x >> 1, self.y >> 1))
        }
    }

    /// The ancestor tile at the given (lower) zoom, or `None` if `z`
    /// is greater than or equal to this tile's zoom.
    #[inline]
    pub fn ancestor_at(self, z: u8) -> Option<TileId> {
        if z >= self.z {
            return None;
        }
        let dz = self.z - z;
        Some(TileId::new(z, self.x >> dz, self.y >> dz))
    }

    /// `true` iff `other` lies inside this tile's spatial bounds at a
    /// deeper zoom. A tile is *not* considered its own ancestor.
    pub fn is_ancestor_of(self, other: TileId) -> bool {
        if other.z <= self.z {
            return false;
        }
        let dz = other.z - self.z;
        other.x >> dz == self.x && other.y >> dz == self.y
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

/// Convert a position in tile pixels to world unit-square coordinates.
///
/// `px` / `py` are measured from the tile's own top-left corner in the
/// canvas's pixel grid, so a padded canvas passes negative values for the
/// margin above and left of the tile. `tile_w` / `tile_h` are the tile's
/// pixel dimensions, taken separately because a canvas may be rectangular.
///
/// This is [`tile_to_world`] for consumers that have already scaled
/// geometry into pixels; both are exact for power-of-two tile sizes, so two
/// tiles naming the same point agree bit for bit.
#[inline]
pub fn tile_px_to_world(tile: TileId, px: f64, py: f64, tile_w: f64, tile_h: f64) -> WorldPos {
    let n = tile.axis_tiles() as f64;
    WorldPos {
        x: (tile.x as f64 + px / tile_w) / n,
        y: (tile.y as f64 + py / tile_h) / n,
    }
}

/// Length of the equator on the WGS-84 ellipsoid, in metres. One world
/// unit of x spans this at the equator.
pub const EARTH_CIRCUMFERENCE_M: f64 = 40_075_016.685_578_5;

/// The latitude Web Mercator runs out at, in degrees. The projection
/// sends the poles to infinity, so the world square stops here.
pub const MERCATOR_MAX_LAT: f64 = 85.051_128_779_8;

/// World x of a longitude in degrees. `-180` is `0.0`, `180` is `1.0`.
#[inline]
pub fn lon_to_world_x(lon_deg: f64) -> f64 {
    (lon_deg + 180.0) / 360.0
}

/// World y of a latitude in degrees, clamped to the Mercator domain.
/// North is `0.0`, south is `1.0`, matching the y-down tile grid.
#[inline]
pub fn lat_to_world_y(lat_deg: f64) -> f64 {
    let lat = lat_deg
        .clamp(-MERCATOR_MAX_LAT, MERCATOR_MAX_LAT)
        .to_radians();
    (1.0 - lat.tan().asinh() / std::f64::consts::PI) / 2.0
}

/// Longitude in degrees of a world x. Inverse of [`lon_to_world_x`].
#[inline]
pub fn world_x_to_lon(wx: f64) -> f64 {
    wx * 360.0 - 180.0
}

/// Latitude in degrees of a world y. Inverse of [`lat_to_world_y`].
#[inline]
pub fn world_y_to_lat(wy: f64) -> f64 {
    (std::f64::consts::PI * (1.0 - 2.0 * wy))
        .sinh()
        .atan()
        .to_degrees()
}

/// Ground metres per world unit at world y `wy`.
///
/// Web Mercator inflates distances away from the equator by `1 / cos(lat)`,
/// and is conformal, so the same factor applies along both axes: a world
/// unit square at `wy` covers `metres_per_world_unit(wy).powi(2)` square
/// metres of ground. Callers converting a real-world density (people per
/// km², say) into one expressed in world or tile-pixel units need this.
#[inline]
pub fn metres_per_world_unit(wy: f64) -> f64 {
    // lat = atan(sinh(pi (1 - 2 wy))), and cos(atan(sinh(u))) = 1/cosh(u),
    // so the cosine falls out without the round trip through a latitude.
    let u = std::f64::consts::PI * (1.0 - 2.0 * wy);
    EARTH_CIRCUMFERENCE_M / u.cosh()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_walks_one_level() {
        assert_eq!(TileId::new(0, 0, 0).parent(), None);
        assert_eq!(TileId::new(3, 5, 6).parent(), Some(TileId::new(2, 2, 3)));
    }

    #[test]
    fn ancestor_at_handles_invalid() {
        let t = TileId::new(5, 10, 20);
        assert_eq!(t.ancestor_at(5), None); // same zoom → not an ancestor
        assert_eq!(t.ancestor_at(6), None); // deeper zoom
        assert_eq!(t.ancestor_at(3), Some(TileId::new(3, 2, 5)));
        assert_eq!(t.ancestor_at(0), Some(TileId::new(0, 0, 0)));
    }

    #[test]
    fn is_ancestor_of() {
        let parent = TileId::new(5, 10, 20);
        assert!(parent.is_ancestor_of(TileId::new(7, 41, 81))); // inside
        assert!(!parent.is_ancestor_of(TileId::new(7, 44, 81))); // wrong x branch
        assert!(!parent.is_ancestor_of(parent)); // not self
        assert!(!parent.is_ancestor_of(TileId::new(4, 5, 10))); // ancestor, not descendant
    }

    #[test]
    fn tile_px_to_world_agrees_with_tile_to_world() {
        let tile = TileId::new(6, 20, 41);
        // The same point, expressed in extent units and in tile pixels.
        for (frac, extent, tile_px) in [
            (0.0, 4096.0, 512.0),
            (0.25, 4096.0, 512.0),
            (0.75, 4096.0, 256.0),
        ] {
            let a = tile_to_world(tile, frac * extent, frac * extent, extent);
            let b = tile_px_to_world(tile, frac * tile_px, frac * tile_px, tile_px, tile_px);
            assert_eq!(a.x, b.x, "x at {frac}");
            assert_eq!(a.y, b.y, "y at {frac}");
        }
    }

    #[test]
    fn tile_px_to_world_scales_each_axis_independently() {
        // A rectangular canvas: y must divide by the tile's own height.
        let tile = TileId::new(3, 1, 2);
        let p = tile_px_to_world(tile, 128.0, 64.0, 256.0, 128.0);
        assert_eq!(p.x, (1.0 + 0.5) / 8.0);
        assert_eq!(p.y, (2.0 + 0.5) / 8.0);
    }

    #[test]
    fn tile_px_to_world_takes_the_padding_margin() {
        // A padded canvas on the top-left tile reaches above and left of the
        // world square; the conversion extrapolates rather than clamping.
        let p = tile_px_to_world(TileId::new(2, 0, 0), -16.0, -16.0, 512.0, 512.0);
        assert!(p.x < 0.0 && p.y < 0.0, "{p:?}");
    }

    #[test]
    fn lon_lat_round_trip_through_world_coords() {
        for lon in [-180.0, -74.0, 0.0, 139.7, 180.0] {
            let wx = lon_to_world_x(lon);
            assert!((world_x_to_lon(wx) - lon).abs() < 1e-9, "lon {lon}");
        }
        for lat in [-80.0, -35.7, 0.0, 51.5, 80.0] {
            let wy = lat_to_world_y(lat);
            assert!((world_y_to_lat(wy) - lat).abs() < 1e-9, "lat {lat}");
        }
        // North is y = 0 and the equator is the middle of the square.
        assert!(lat_to_world_y(80.0) < lat_to_world_y(-80.0));
        assert!((lat_to_world_y(0.0) - 0.5).abs() < 1e-12);
        // Beyond the projection's domain the value saturates.
        assert_eq!(lat_to_world_y(89.0), lat_to_world_y(MERCATOR_MAX_LAT));
    }

    #[test]
    fn metres_per_world_unit_matches_mercator_scale() {
        // The equator is the unscaled reference.
        assert!((metres_per_world_unit(0.5) - EARTH_CIRCUMFERENCE_M).abs() < 1.0);
        // 60°N sits at wy where cos(lat) = 1/2, so the scale halves.
        let wy_60n = (1.0 - 60f64.to_radians().tan().asinh() / std::f64::consts::PI) / 2.0;
        let ratio = metres_per_world_unit(wy_60n) / EARTH_CIRCUMFERENCE_M;
        assert!((ratio - 0.5).abs() < 1e-9, "ratio {ratio}");
        // Symmetric about the equator.
        assert!((metres_per_world_unit(0.2) - metres_per_world_unit(0.8)).abs() < 1e-6);
    }
}
