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
