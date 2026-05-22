//! Convex hull via the [`geo`] crate. Accepts an arbitrary mix of
//! points, polylines, and polygon rings — all vertices are pooled and
//! fed to [`MultiPoint::convex_hull`].

use geo::{ConvexHull, Coord, MultiPoint, Point};

use crate::Polygon;

use super::convert::pt_to_i;

/// Compute the convex hull of every input vertex. Returns `None` if
/// fewer than 3 distinct points are supplied.
pub fn convex_hull(
    points: &[(i32, i32)],
    lines: &[Vec<(i32, i32)>],
    polygons: &[Polygon],
) -> Option<Polygon> {
    let mut coords = Vec::new();
    for &(x, y) in points {
        coords.push(Point::from(Coord {
            x: x as f64,
            y: y as f64,
        }));
    }
    for line in lines {
        for &(x, y) in line {
            coords.push(Point::from(Coord {
                x: x as f64,
                y: y as f64,
            }));
        }
    }
    for p in polygons {
        for &(x, y) in &p.exterior {
            coords.push(Point::from(Coord {
                x: x as f64,
                y: y as f64,
            }));
        }
    }
    if coords.len() < 3 {
        return None;
    }
    let mp = MultiPoint::from(coords);
    let hull = mp.convex_hull();
    let exterior: Vec<(i32, i32)> = hull
        .exterior()
        .0
        .iter()
        .map(|c| pt_to_i([c.x, c.y]))
        .collect();
    (exterior.len() >= 4).then(|| Polygon {
        exterior,
        holes: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_hull() {
        let pts = vec![(0, 0), (10, 0), (10, 10), (0, 10), (5, 5)];
        let h = convex_hull(&pts, &[], &[]).unwrap();
        assert_eq!(h.exterior.len(), 5);
    }
}
