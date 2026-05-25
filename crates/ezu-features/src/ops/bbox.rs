//! Axis-aligned bounding box (envelope) over a heterogeneous feature
//! pool. Pools every vertex from polygons + lines + points and emits
//! a single rectangular `Polygon` covering them.

use crate::Polygon;

/// Bounding box `(min_x, min_y, max_x, max_y)` over all input vertices.
/// Returns `None` if every input collection is empty.
pub fn bbox(
    points: &[(i32, i32)],
    lines: &[Vec<(i32, i32)>],
    polygons: &[Polygon],
) -> Option<(i32, i32, i32, i32)> {
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    let mut any = false;
    let mut take = |x: i32, y: i32| {
        any = true;
        if x < min_x {
            min_x = x;
        }
        if y < min_y {
            min_y = y;
        }
        if x > max_x {
            max_x = x;
        }
        if y > max_y {
            max_y = y;
        }
    };
    for &(x, y) in points {
        take(x, y);
    }
    for line in lines {
        for &(x, y) in line {
            take(x, y);
        }
    }
    for poly in polygons {
        for &(x, y) in &poly.exterior {
            take(x, y);
        }
        for hole in &poly.holes {
            for &(x, y) in hole {
                take(x, y);
            }
        }
    }
    if !any {
        return None;
    }
    Some((min_x, min_y, max_x, max_y))
}

/// Same as [`bbox`] but returns a single closed rectangular polygon
/// in CCW order, or `None` for an empty pool.
pub fn bbox_polygon(
    points: &[(i32, i32)],
    lines: &[Vec<(i32, i32)>],
    polygons: &[Polygon],
) -> Option<Polygon> {
    let (min_x, min_y, max_x, max_y) = bbox(points, lines, polygons)?;
    Some(Polygon {
        exterior: vec![
            (min_x, min_y),
            (max_x, min_y),
            (max_x, max_y),
            (min_x, max_y),
        ],
        holes: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_covers_all_inputs() {
        let polys = vec![Polygon {
            exterior: vec![(0, 0), (10, 0), (10, 10), (0, 10)],
            holes: vec![],
        }];
        let lines: Vec<Vec<(i32, i32)>> = vec![vec![(-5, 5), (5, 15)]];
        let points = vec![(20, -3)];
        let (min_x, min_y, max_x, max_y) = bbox(&points, &lines, &polys).unwrap();
        assert_eq!((min_x, min_y, max_x, max_y), (-5, -3, 20, 15));
    }

    #[test]
    fn bbox_empty_returns_none() {
        let polys: Vec<Polygon> = vec![];
        let lines: Vec<Vec<(i32, i32)>> = vec![];
        let points: Vec<(i32, i32)> = vec![];
        assert!(bbox(&points, &lines, &polys).is_none());
    }
}
