//! Boundary of a polygon — the exterior ring and every interior ring
//! as separate `LineString`s. Output is suitable for stroking polygons
//! as outlines or feeding into line-only paint nodes.

use crate::Polygon;

/// Returns one polyline per ring (exterior first, then holes).
pub fn polygon_boundary(p: &Polygon) -> Vec<Vec<(i32, i32)>> {
    let mut out = Vec::with_capacity(1 + p.holes.len());
    if !p.exterior.is_empty() {
        out.push(p.exterior.clone());
    }
    for h in &p.holes {
        if !h.is_empty() {
            out.push(h.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_yields_exterior_then_holes() {
        let p = Polygon {
            exterior: vec![(0, 0), (10, 0), (10, 10), (0, 10), (0, 0)],
            holes: vec![vec![(2, 2), (4, 2), (4, 4), (2, 4), (2, 2)]],
        };
        let rings = polygon_boundary(&p);
        assert_eq!(rings.len(), 2);
        assert_eq!(rings[0].len(), 5);
        assert_eq!(rings[1].len(), 5);
    }
}
