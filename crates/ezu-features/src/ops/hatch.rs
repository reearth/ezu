//! Parallel-line hatching: fill polygons with a set of equally-spaced
//! lines at a given angle. Output is the clipped polyline pieces that
//! fall inside the polygon, suitable for stroking.
//!
//! Implementation: generate a family of parallel lines spanning the
//! polygon's axis-aligned bounding box (rotated into the hatch frame),
//! then clip them against the polygon using
//! [`i_overlay::float::clip::FloatClip`].

use i_overlay::core::fill_rule::FillRule;
use i_overlay::float::clip::FloatClip;
use i_overlay::string::clip::ClipRule;

use crate::Polygon;

use super::convert::{line_to_i, polygon_to_f};

/// Hatching parameters.
#[derive(Debug, Clone, Copy)]
pub struct HatchOpts {
    /// Hatch direction in degrees, measured counter-clockwise from
    /// the +X axis. `0.0` produces horizontal lines.
    pub angle_deg: f64,
    /// Perpendicular spacing between consecutive lines, in input
    /// coordinate units (tile pixels).
    pub spacing: f64,
    /// Per-line phase offset, in spacing units. `0.0` snaps the first
    /// line through the polygon centroid's projection.
    pub phase: f64,
    /// World-space offset of the polygon coordinate frame, in the same
    /// units as the polygon vertices. The hatch line family is laid
    /// out in `(local + origin)` so adjacent tiles agree on which
    /// world lines exist (no seam at tile borders). Default `(0, 0)`
    /// reproduces the legacy tile-local behavior.
    pub origin: (f64, f64),
}

impl Default for HatchOpts {
    fn default() -> Self {
        Self {
            angle_deg: 0.0,
            spacing: 8.0,
            phase: 0.0,
            origin: (0.0, 0.0),
        }
    }
}

/// Produce hatch polylines covering every polygon. Each input polygon
/// is hatched independently and the resulting clipped segments are
/// concatenated. Returns an empty `Vec` when `spacing <= 0` or no
/// polygons are supplied.
pub fn hatch_polygons(polys: &[Polygon], opts: &HatchOpts) -> Vec<Vec<(i32, i32)>> {
    if polys.is_empty() || !opts.spacing.is_finite() || opts.spacing <= 0.0 {
        return Vec::new();
    }
    let theta = opts.angle_deg.to_radians();
    let (sin, cos) = theta.sin_cos();
    // Project the world origin onto the hatch normal — `v` in world
    // space is `v_local + v_origin`, so aligning v0 to a world grid
    // means picking `v0_local = ceil((v_min + v_origin)/spacing) *
    // spacing - v_origin`.
    let (ox, oy) = opts.origin;
    let v_origin = -ox * sin + oy * cos;
    // Rotation aligning the hatch direction with the local +X axis is
    // (cos, sin; -sin, cos). We project polygon vertices onto the
    // perpendicular (the local Y axis) to find the slab range.
    let mut out = Vec::new();
    for poly in polys {
        if poly.exterior.is_empty() {
            continue;
        }
        // Slab range along the hatch normal: project every exterior
        // vertex onto (-sin, cos).
        let mut v_min = f64::INFINITY;
        let mut v_max = f64::NEG_INFINITY;
        // Also need the rotated bbox along the hatch direction to know
        // how far to extend each line.
        let mut u_min = f64::INFINITY;
        let mut u_max = f64::NEG_INFINITY;
        for &(x, y) in &poly.exterior {
            let xf = x as f64;
            let yf = y as f64;
            let u = xf * cos + yf * sin;
            let v = -xf * sin + yf * cos;
            v_min = v_min.min(v);
            v_max = v_max.max(v);
            u_min = u_min.min(u);
            u_max = u_max.max(u);
        }
        if !v_min.is_finite() || v_max <= v_min {
            continue;
        }
        // Snap to the world-aligned line family. With v_origin == 0
        // this reduces to the legacy local snap.
        let v0 = ((v_min + v_origin) / opts.spacing).floor() * opts.spacing - v_origin
            + opts.phase * opts.spacing;
        // Generate the parallel-line family in tile-local coords. Each
        // line is two endpoints crossing the polygon's rotated bbox.
        let mut lines: Vec<Vec<[f64; 2]>> = Vec::new();
        let mut v = v0;
        // Step backwards once if the phase pushed us above v_min so we
        // don't miss the first slice.
        if v > v_min {
            v -= opts.spacing;
        }
        while v <= v_max + opts.spacing {
            // Endpoint in (u, v) space, rotated back to world.
            let a_world = [u_min * cos - v * sin, u_min * sin + v * cos];
            let b_world = [u_max * cos - v * sin, u_max * sin + v * cos];
            lines.push(vec![a_world, b_world]);
            v += opts.spacing;
        }
        if lines.is_empty() {
            continue;
        }
        let shape = polygon_to_f(poly);
        let clip_rule = ClipRule {
            invert: false,
            boundary_included: false,
        };
        let clipped = lines.clip_by(&shape, FillRule::EvenOdd, clip_rule);
        for path in &clipped {
            if path.len() >= 2 {
                out.push(line_to_i(path));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_hatch_fills_square() {
        let p = Polygon {
            exterior: vec![(0, 0), (100, 0), (100, 100), (0, 100), (0, 0)],
            holes: vec![],
        };
        let out = hatch_polygons(
            &[p],
            &HatchOpts {
                angle_deg: 0.0,
                spacing: 10.0,
                phase: 0.0,
                origin: (0.0, 0.0),
            },
        );
        // ~10 horizontal lines through a 100-tall square.
        assert!(out.len() >= 8 && out.len() <= 12, "got {} lines", out.len());
        for line in &out {
            // each clipped segment is a 2-point polyline of width 100.
            assert_eq!(line.len(), 2);
        }
    }

    #[test]
    fn zero_spacing_is_no_op() {
        let p = Polygon {
            exterior: vec![(0, 0), (10, 0), (10, 10), (0, 10), (0, 0)],
            holes: vec![],
        };
        let out = hatch_polygons(
            &[p],
            &HatchOpts {
                angle_deg: 0.0,
                spacing: 0.0,
                phase: 0.0,
                origin: (0.0, 0.0),
            },
        );
        assert!(out.is_empty());
    }
}
