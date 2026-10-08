//! Centroid of a polygon or polyline.
//!
//! - Polygon: area-weighted centroid using the signed-area shoelace
//!   formula on the exterior ring. Holes are ignored (good enough for
//!   cartographic labelling; switch to a hole-aware version if needed).
//!   [`polygon_area_centroid`] is that hole-aware version.
//! - LineString: length-weighted midpoint across all segments.
//!
//! Both return `None` when the input is degenerate (no area / no
//! length).

use crate::Polygon;

/// Area-weighted centroid of a polygon's exterior ring.
pub fn polygon_centroid(p: &Polygon) -> Option<(i32, i32)> {
    centroid_ring(&p.exterior)
}

/// Area-weighted centroid of a polygon with its holes cut out, unrounded.
/// Rings count by their area whichever way they wind.
///
/// The sums run relative to the first exterior vertex, so a polygon moved
/// by whole units gives the same point moved by the same amount, and the
/// products stay small however far from the origin the polygon sits.
/// `None` when the polygon has no area.
pub fn polygon_area_centroid(p: &Polygon) -> Option<(f64, f64)> {
    let &(ox, oy) = p.exterior.first()?;
    // Twice the area, and the first moments times six, of one ring.
    let moments = |ring: &[(i32, i32)]| -> (f64, f64, f64) {
        let local = |&(x, y): &(i32, i32)| {
            (
                (i64::from(x) - i64::from(ox)) as f64,
                (i64::from(y) - i64::from(oy)) as f64,
            )
        };
        let (mut a2, mut mx, mut my) = (0.0, 0.0, 0.0);
        for (i, a) in ring.iter().enumerate() {
            let (x0, y0) = local(a);
            let (x1, y1) = local(&ring[(i + 1) % ring.len()]);
            let cross = x0 * y1 - x1 * y0;
            a2 += cross;
            mx += (x0 + x1) * cross;
            my += (y0 + y1) * cross;
        }
        // Wound the other way, the same ring has the same moments negated.
        let s = if a2 < 0.0 { -1.0 } else { 1.0 };
        (a2 * s, mx * s, my * s)
    };
    let (mut a2, mut mx, mut my) = moments(&p.exterior);
    for hole in &p.holes {
        let (h2, hx, hy) = moments(hole);
        a2 -= h2;
        mx -= hx;
        my -= hy;
    }
    if a2 <= 0.0 {
        return None;
    }
    Some((
        mx / (3.0 * a2) + f64::from(ox),
        my / (3.0 * a2) + f64::from(oy),
    ))
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
        let (sx, sy) = ring.iter().fold((0.0_f64, 0.0_f64), |(a, b), p| {
            (a + p.0 as f64, b + p.1 as f64)
        });
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
        assert_eq!(polygon_area_centroid(&p), None);
    }

    #[test]
    fn area_centroid_cuts_out_holes_however_rings_wind() {
        let square = vec![(0, 0), (40, 0), (40, 40), (0, 40)];
        let p = Polygon {
            exterior: square.clone(),
            holes: vec![],
        };
        assert_eq!(polygon_area_centroid(&p), Some((20.0, 20.0)));
        // A hole in the east half pulls the centroid west: 1600 − 400 of
        // area, with the hole's moment at x = 30 taken out.
        for hole in [
            vec![(20, 10), (40, 10), (40, 30), (20, 30)],
            vec![(20, 10), (20, 30), (40, 30), (40, 10)],
        ] {
            let p = Polygon {
                exterior: square.clone(),
                holes: vec![hole],
            };
            let (x, y) = polygon_area_centroid(&p).unwrap();
            assert!((x - (1600.0 * 20.0 - 400.0 * 30.0) / 1200.0).abs() < 1e-9);
            assert!((y - 20.0).abs() < 1e-9);
        }
    }

    #[test]
    fn area_centroid_moves_with_its_polygon() {
        let ring = [(3878, 539), (3941, 590), (3796, 784), (3723, 719)];
        let at = |dx: i32| Polygon {
            exterior: ring.iter().map(|&(x, y)| (x + dx, y)).collect(),
            holes: vec![],
        };
        let (x0, y0) = polygon_area_centroid(&at(0)).unwrap();
        let (x1, y1) = polygon_area_centroid(&at(-131072)).unwrap();
        assert_eq!((x0 - 131072.0, y0), (x1, y1));
    }
}
