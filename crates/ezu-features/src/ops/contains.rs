//! Point-in-polygon tests shared by the ops that need to know whether a
//! generated point lands inside a feature (`scatter`, `voronoi`).

use crate::Polygon;

/// Ray-casting point-in-polygon. Treats holes correctly: a point in a
/// hole counts as outside.
pub fn point_in_polygon(p: &Polygon, x: f64, y: f64) -> bool {
    if !ring_contains(&p.exterior, x, y) {
        return false;
    }
    for hole in &p.holes {
        if ring_contains(hole, x, y) {
            return false;
        }
    }
    true
}

/// Ray-casting containment for a single ring. Rings with fewer than
/// three vertices enclose nothing.
pub fn ring_contains(ring: &[(i32, i32)], x: f64, y: f64) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let mut inside = false;
    let n = ring.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = (ring[i].0 as f64, ring[i].1 as f64);
        let (xj, yj) = (ring[j].0 as f64, ring[j].1 as f64);
        let intersect =
            ((yi > y) != (yj > y)) && (x < (xj - xi) * (y - yi) / (yj - yi + f64::EPSILON) + xi);
        if intersect {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<(i32, i32)> {
        vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)]
    }

    #[test]
    fn inside_outside_and_holes() {
        let p = Polygon {
            exterior: square(0, 0, 100, 100),
            holes: vec![square(40, 40, 60, 60)],
        };
        assert!(point_in_polygon(&p, 10.0, 10.0));
        assert!(!point_in_polygon(&p, 150.0, 10.0));
        assert!(!point_in_polygon(&p, 50.0, 50.0), "hole counts as outside");
    }

    #[test]
    fn degenerate_ring_encloses_nothing() {
        assert!(!ring_contains(&[(0, 0), (10, 10)], 5.0, 5.0));
    }
}
