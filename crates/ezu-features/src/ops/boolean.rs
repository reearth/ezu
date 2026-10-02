//! Polygon-vs-polygon set ops (union / intersection / difference /
//! symmetric difference) via `i_overlay`'s float overlay.

use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::single::SingleFloatOverlay;

use crate::Polygon;

use super::convert::{polygon_to_f, polygons_from_shapes};

#[derive(Debug, Clone, Copy)]
pub enum BoolOp {
    Union,
    Intersection,
    Difference,
    SymmetricDifference,
}

impl BoolOp {
    fn to_overlay_rule(self) -> OverlayRule {
        match self {
            BoolOp::Union => OverlayRule::Union,
            BoolOp::Intersection => OverlayRule::Intersect,
            BoolOp::Difference => OverlayRule::Difference,
            BoolOp::SymmetricDifference => OverlayRule::Xor,
        }
    }
}

/// Apply `op` to two polygon sets and return the resulting polygons.
/// Subject = `a`, clip = `b`. EvenOdd fill rule so holes and
/// multi-ring inputs survive the round-trip.
pub fn polygon_boolean(a: &[Polygon], b: &[Polygon], op: BoolOp) -> Vec<Polygon> {
    if a.is_empty() && b.is_empty() {
        return Vec::new();
    }
    // i_overlay's `overlay` wants a `Subject` (vec of contour-sets =
    // polygons) and `Clip` (same shape). Flatten each polygon to a
    // list of paths and pass as the float shapes.
    let subj: Vec<Vec<Vec<[f64; 2]>>> = a.iter().map(polygon_to_f).collect();
    let clip: Vec<Vec<Vec<[f64; 2]>>> = b.iter().map(polygon_to_f).collect();
    let result = subj.overlay(&clip, op.to_overlay_rule(), FillRule::EvenOdd);
    polygons_from_shapes(&result)
}

/// Dissolve a polygon set into its union: overlaps fill once and edges
/// two polygons share disappear.
///
/// Unlike [`polygon_boolean`]'s even-odd rule, where two overlapping
/// inputs cancel to a hole, every ring is first wound by role (exteriors
/// one way, holes the other) and the overlay counts windings, so any
/// point covered by some polygon's interior is filled — whichever way
/// the source wound its rings.
pub fn polygon_union_all(polys: &[Polygon]) -> Vec<Polygon> {
    if polys.is_empty() {
        return Vec::new();
    }
    let wound = |ring: &[(i32, i32)], positive: bool| -> Vec<[f64; 2]> {
        let mut path: Vec<[f64; 2]> = ring.iter().map(|&(x, y)| [x as f64, y as f64]).collect();
        if (signed_area(&path) > 0.0) != positive {
            path.reverse();
        }
        path
    };
    let subj: Vec<Vec<Vec<[f64; 2]>>> = polys
        .iter()
        .map(|p| {
            let mut shape = vec![wound(&p.exterior, true)];
            shape.extend(p.holes.iter().map(|h| wound(h, false)));
            shape
        })
        .collect();
    let none: Vec<Vec<Vec<[f64; 2]>>> = Vec::new();
    let result = subj.overlay(&none, OverlayRule::Union, FillRule::NonZero);
    polygons_from_shapes(&result)
}

/// Twice the signed area of a ring (shoelace); the sign is its winding.
fn signed_area(ring: &[[f64; 2]]) -> f64 {
    let n = ring.len();
    (0..n)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: i32, y0: i32, x1: i32, y1: i32) -> Polygon {
        Polygon {
            exterior: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)],
            holes: vec![],
        }
    }

    #[test]
    fn union_of_overlapping_rects_yields_one_polygon() {
        let a = vec![rect(0, 0, 10, 10)];
        let b = vec![rect(5, 5, 15, 15)];
        let u = polygon_boolean(&a, &b, BoolOp::Union);
        assert_eq!(u.len(), 1, "union should merge into 1 polygon: {u:?}");
    }

    #[test]
    fn union_all_fills_overlaps_whatever_the_winding() {
        // The same square twice, wound opposite ways: even-odd would
        // cancel them, the union keeps one square.
        let a = rect(0, 0, 10, 10);
        let mut b = rect(0, 0, 10, 10);
        b.exterior.reverse();
        let u = polygon_union_all(&[a, b]);
        assert_eq!(u.len(), 1, "{u:?}");
        assert!(u[0].holes.is_empty());
    }

    #[test]
    fn union_all_dissolves_a_shared_edge() {
        let u = polygon_union_all(&[rect(0, 0, 10, 10), rect(10, 0, 20, 10)]);
        assert_eq!(u.len(), 1, "{u:?}");
        assert!(
            u[0].exterior.iter().all(|&(x, _)| x != 10),
            "no vertex may stay on the shared edge: {u:?}"
        );
    }

    #[test]
    fn intersection_returns_overlap_region() {
        let a = vec![rect(0, 0, 10, 10)];
        let b = vec![rect(5, 5, 15, 15)];
        let i = polygon_boolean(&a, &b, BoolOp::Intersection);
        assert_eq!(i.len(), 1);
        // The overlap rect is roughly 5,5 -> 10,10 — check bounding extent.
        let mut min_x = i32::MAX;
        let mut max_x = i32::MIN;
        for &(x, _) in &i[0].exterior {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
        }
        assert_eq!((min_x, max_x), (5, 10));
    }

    #[test]
    fn difference_removes_b_from_a() {
        let a = vec![rect(0, 0, 10, 10)];
        let b = vec![rect(5, 0, 15, 10)];
        let d = polygon_boolean(&a, &b, BoolOp::Difference);
        assert_eq!(d.len(), 1);
        let mut max_x = i32::MIN;
        for &(x, _) in &d[0].exterior {
            max_x = max_x.max(x);
        }
        assert_eq!(max_x, 5);
    }
}
