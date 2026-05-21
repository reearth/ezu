//! Centroid of a polygon or polyline.
//!
//! - Polygon: area-weighted centroid using the signed-area shoelace
//!   formula on the exterior ring. Holes are ignored (good enough for
//!   cartographic labelling; switch to a hole-aware version if needed).
//! - LineString: length-weighted midpoint across all segments.
//!
//! Both return `None` when the input is degenerate (no area / no
//! length).

use crate::Polygon;

/// Area-weighted centroid of a polygon's exterior ring.
pub fn polygon_centroid(p: &Polygon) -> Option<(i32, i32)> {
    centroid_ring(&p.exterior)
}

/// Length-weighted centroid of a polyline.
pub fn linestring_centroid(line: &[(i32, i32)]) -> Option<(i32, i32)> {
    if line.len() < 2 {
        return line.first().copied();
    }
    let mut total = 0.0_f64;
    let mut cx = 0.0_f64;
    let mut cy = 0.0_f64;
    for w in line.windows(2) {
        let (x0, y0) = (w[0].0 as f64, w[0].1 as f64);
        let (x1, y1) = (w[1].0 as f64, w[1].1 as f64);
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = (dx * dx + dy * dy).sqrt();
        if len == 0.0 {
            continue;
        }
        total += len;
        cx += (x0 + x1) * 0.5 * len;
        cy += (y0 + y1) * 0.5 * len;
    }
    if total == 0.0 {
        return None;
    }
    Some(((cx / total).round() as i32, (cy / total).round() as i32))
}

fn centroid_ring(ring: &[(i32, i32)]) -> Option<(i32, i32)> {
    if ring.len() < 3 {
        return None;
    }
    let mut a2 = 0.0_f64;
    let mut cx = 0.0_f64;
    let mut cy = 0.0_f64;
    let n = ring.len();
    for i in 0..n {
        let (x0, y0) = (ring[i].0 as f64, ring[i].1 as f64);
        let (x1, y1) = (ring[(i + 1) % n].0 as f64, ring[(i + 1) % n].1 as f64);
        let cross = x0 * y1 - x1 * y0;
        a2 += cross;
        cx += (x0 + x1) * cross;
        cy += (y0 + y1) * cross;
    }
    if a2 == 0.0 {
        // Degenerate (collinear) ring — fall back to vertex average so
        // labelling code still gets a point near the geometry.
        let (sx, sy) = ring
            .iter()
            .fold((0.0_f64, 0.0_f64), |(a, b), p| (a + p.0 as f64, b + p.1 as f64));
        let n = ring.len() as f64;
        return Some(((sx / n).round() as i32, (sy / n).round() as i32));
    }
    let factor = 1.0 / (3.0 * a2);
    Some(((cx * factor).round() as i32, (cy * factor).round() as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_centroid_is_center() {
        let p = Polygon {
            exterior: vec![(0, 0), (10, 0), (10, 10), (0, 10), (0, 0)],
            holes: vec![],
        };
        assert_eq!(polygon_centroid(&p), Some((5, 5)));
    }

    #[test]
    fn linestring_midpoint() {
        assert_eq!(linestring_centroid(&[(0, 0), (10, 0)]), Some((5, 0)));
    }

    #[test]
    fn degenerate_polygon_returns_none() {
        let p = Polygon {
            exterior: vec![(0, 0), (1, 0)],
            holes: vec![],
        };
        assert_eq!(polygon_centroid(&p), None);
    }
}
