//! Density-driven point scatter: fill polygons with points whose *areal
//! density* matches a caller-supplied target, one point per lattice cell
//! at most.
//!
//! # Why a lattice, not a count
//!
//! The obvious way to place `n` dots in a polygon is rejection sampling:
//! draw uniform points in the bounding box until `n` land inside. That
//! cannot work here. Tile geometry is clipped, so a feature spanning four
//! tiles arrives as four separate polygons and each would get its own `n`
//! dots — four times too many, with a visible discontinuity at every
//! seam.
//!
//! So the scatter is anchored in world space instead. The world is cut
//! into square cells of side `spacing`; each cell offers at most one
//! point, and its position and its keep-or-drop decision are functions of
//! the cell's integer world index alone. Two tiles covering the same cell
//! therefore reach the same answer, and the pattern is continuous across
//! seams and stable as the viewport moves. What the caller controls is
//! the expected number of points per unit area, not the exact count.

use ezu_core::seed::{cell_seed, next_unit};

use super::contains::point_in_polygon;
use crate::Polygon;

/// Scatter parameters. Lengths are in the same units as the polygon
/// vertices (tile pixels).
#[derive(Debug, Clone, Copy)]
pub struct ScatterOpts {
    /// Side of one lattice cell. Also the ceiling on density: a cell
    /// holds at most one point, so no more than `1 / spacing²` points
    /// per unit area can be emitted however high the target goes.
    pub spacing: f64,
    /// How far a point may stray from its cell centre, as a fraction of
    /// the cell. `0.0` leaves a visible grid; `1.0` lets the point land
    /// anywhere in its cell and reads as random.
    pub jitter: f64,
    /// World-space offset of the polygon coordinate frame, in vertex
    /// units — for a tile, `(tile.x * extent, tile.y * extent)`. The
    /// lattice is laid out in `(local + origin)`, which is what makes
    /// adjacent tiles agree. `(0, 0)` anchors it to the tile instead.
    pub origin: (f64, f64),
    /// Salt for the per-cell seed, so two scatters over the same
    /// polygons (different attributes, say) don't land on top of each
    /// other.
    pub salt: u32,
}

impl Default for ScatterOpts {
    fn default() -> Self {
        Self {
            spacing: 8.0,
            jitter: 1.0,
            origin: (0.0, 0.0),
            salt: 0,
        }
    }
}

/// Salt distinguishing the cell position draw from anything else seeded
/// off the same lattice.
const SCATTER_SALT: u32 = 0xD0_7D_E4_51;

/// Scatter points across `polys`, treated as one multi-polygon: a point
/// is kept when it falls inside any of them and outside every hole of
/// the polygon that contains it, so overlapping rings do not double up.
///
/// `density_at` receives a **world** y (vertex units, `local + origin.1`)
/// and returns the target points per square vertex-unit for that row.
/// Taking it per row rather than as a constant is what lets a caller
/// correct for Web Mercator's latitude-dependent scale without breaking
/// continuity — the correction is a function of world position, so
/// neighbouring tiles still agree.
///
/// Returns an empty `Vec` when `spacing` is not positive or no polygon
/// has an interior.
pub fn scatter_polygons<F>(polys: &[Polygon], opts: &ScatterOpts, density_at: F) -> Vec<(i32, i32)>
where
    F: Fn(f64) -> f64,
{
    if polys.is_empty() || !opts.spacing.is_finite() || opts.spacing <= 0.0 {
        return Vec::new();
    }
    // Per-polygon bounds, to skip whole polygons per cell cheaply, plus
    // the union to bound the lattice sweep.
    let bounds: Vec<[f64; 4]> = polys.iter().map(ring_bounds).collect();
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for b in &bounds {
        min_x = min_x.min(b[0]);
        min_y = min_y.min(b[1]);
        max_x = max_x.max(b[2]);
        max_y = max_y.max(b[3]);
    }
    if !(min_x <= max_x && min_y <= max_y) {
        return Vec::new();
    }

    let spacing = opts.spacing;
    let cell_area = spacing * spacing;
    let jitter = opts.jitter.clamp(0.0, 1.0);
    let (ox, oy) = opts.origin;

    // Cell indices are taken in world space, so they name the same cell
    // from whichever tile is asking.
    let i0 = ((min_x + ox) / spacing).floor() as i64;
    let i1 = ((max_x + ox) / spacing).floor() as i64;
    let j0 = ((min_y + oy) / spacing).floor() as i64;
    let j1 = ((max_y + oy) / spacing).floor() as i64;

    let mut out = Vec::new();
    for j in j0..=j1 {
        // Density is a property of the cell, so it is measured at the cell's
        // world centre rather than at wherever the sample jittered to:
        // sampling at the offset point would let a point that strayed towards
        // denser ground raise its own odds of being kept. Being a function of
        // the row alone also hoists it out of the inner loop.
        let world_y_centre = (j as f64 + 0.5) * spacing;
        let keep_p = (density_at(world_y_centre) * cell_area).clamp(0.0, 1.0);
        if keep_p <= 0.0 {
            continue;
        }
        for i in i0..=i1 {
            let mut state = cell_seed(i, j, opts.salt ^ SCATTER_SALT);
            // Fixed draw order: x offset, y offset, then the keep roll.
            let jx = (next_unit(&mut state) as f64 - 0.5) * jitter;
            let jy = (next_unit(&mut state) as f64 - 0.5) * jitter;
            if (next_unit(&mut state) as f64) >= keep_p {
                continue;
            }
            // World cell centre plus jitter, back into local coords.
            let x = (i as f64 + 0.5 + jx) * spacing - ox;
            let y = (j as f64 + 0.5 + jy) * spacing - oy;
            let inside = polys.iter().zip(&bounds).any(|(poly, b)| {
                x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3] && point_in_polygon(poly, x, y)
            });
            if inside {
                out.push((x.round() as i32, y.round() as i32));
            }
        }
    }
    out
}

/// `[min_x, min_y, max_x, max_y]` of a polygon's exterior. An empty
/// exterior yields an inverted box, which never matches a sample.
fn ring_bounds(p: &Polygon) -> [f64; 4] {
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for &(x, y) in &p.exterior {
        let (x, y) = (x as f64, y as f64);
        b[0] = b[0].min(x);
        b[1] = b[1].min(y);
        b[2] = b[2].max(x);
        b[3] = b[3].max(y);
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: i32, y0: i32, x1: i32, y1: i32) -> Polygon {
        Polygon {
            exterior: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)],
            holes: vec![],
        }
    }

    /// Density high enough that every cell is kept, so counts are exact
    /// and the geometry — not the dice — is what's under test.
    fn dense() -> ScatterOpts {
        ScatterOpts {
            spacing: 10.0,
            jitter: 0.0,
            origin: (0.0, 0.0),
            salt: 0,
        }
    }

    #[test]
    fn every_point_lands_inside() {
        let poly = square(0, 0, 200, 200);
        let pts = scatter_polygons(std::slice::from_ref(&poly), &dense(), |_| 1.0);
        assert!(!pts.is_empty());
        for &(x, y) in &pts {
            assert!(
                point_in_polygon(&poly, x as f64, y as f64),
                "({x}, {y}) escaped the polygon"
            );
        }
    }

    #[test]
    fn holes_stay_empty() {
        let poly = Polygon {
            exterior: square(0, 0, 200, 200).exterior,
            holes: vec![square(50, 50, 150, 150).exterior],
        };
        let pts = scatter_polygons(&[poly], &dense(), |_| 1.0);
        assert!(!pts.is_empty());
        for &(x, y) in &pts {
            let in_hole = (50..=150).contains(&x) && (50..=150).contains(&y);
            assert!(!in_hole, "({x}, {y}) landed in the hole");
        }
    }

    #[test]
    fn density_scales_the_count() {
        let poly = square(0, 0, 400, 400);
        let opts = ScatterOpts {
            spacing: 4.0,
            ..dense()
        };
        // Expected counts are area * density: 160000 * 1e-3 = 160 and
        // 160000 * 4e-3 = 640. Sampling noise is a few percent at these
        // counts, so allow a wide band and check the ratio, not the value.
        let sparse = scatter_polygons(std::slice::from_ref(&poly), &opts, |_| 1e-3).len();
        let thick = scatter_polygons(&[poly], &opts, |_| 4e-3).len();
        assert!((100..250).contains(&sparse), "sparse = {sparse}");
        assert!((500..800).contains(&thick), "thick = {thick}");
        assert!(thick > sparse * 2, "{thick} vs {sparse}");
    }

    #[test]
    fn density_ceiling_is_one_per_cell() {
        let poly = square(0, 0, 100, 100);
        let opts = ScatterOpts {
            spacing: 10.0,
            ..dense()
        };
        // 10x10 cells over the square; an absurd density cannot exceed that.
        let pts = scatter_polygons(&[poly], &opts, |_| 1e6);
        assert!(pts.len() <= 121, "{} points", pts.len());
    }

    /// The seam test: a polygon split down the middle and scattered as
    /// two tiles must reproduce exactly the points of the whole.
    #[test]
    fn split_across_tiles_reproduces_the_whole() {
        let extent = 100.0;
        let whole = square(0, 0, 200, 200);
        let opts = ScatterOpts {
            spacing: 7.0,
            jitter: 1.0,
            origin: (0.0, 0.0),
            salt: 3,
        };
        let mut want = scatter_polygons(&[whole], &opts, |_| 5e-3);

        // Tile (0, 0) sees x in [0, 100); tile (1, 0) sees the rest,
        // in its own local frame shifted by one extent.
        let left = scatter_polygons(
            &[square(0, 0, 100, 200)],
            &ScatterOpts {
                origin: (0.0, 0.0),
                ..opts
            },
            |_| 5e-3,
        );
        let right = scatter_polygons(
            &[square(0, 0, 100, 200)],
            &ScatterOpts {
                origin: (extent, 0.0),
                ..opts
            },
            |_| 5e-3,
        );

        let mut got: Vec<(i32, i32)> = left
            .into_iter()
            .chain(right.into_iter().map(|(x, y)| (x + extent as i32, y)))
            .collect();
        got.sort_unstable();
        got.dedup();
        want.sort_unstable();
        want.dedup();
        assert_eq!(got, want);
    }

    #[test]
    fn zero_density_row_is_skipped() {
        let poly = square(0, 0, 200, 200);
        assert!(scatter_polygons(&[poly], &dense(), |_| 0.0).is_empty());
    }

    #[test]
    fn non_positive_spacing_emits_nothing() {
        let poly = square(0, 0, 200, 200);
        let opts = ScatterOpts {
            spacing: 0.0,
            ..dense()
        };
        assert!(scatter_polygons(&[poly], &opts, |_| 1.0).is_empty());
    }

    #[test]
    fn overlapping_polygons_do_not_double_up() {
        let opts = dense();
        let a = square(0, 0, 100, 100);
        let b = square(50, 50, 150, 150);
        let pts = scatter_polygons(&[a, b], &opts, |_| 1.0);
        let mut uniq = pts.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(pts.len(), uniq.len(), "a cell emitted twice");
    }
}
