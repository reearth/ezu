//! Pole of inaccessibility — the point inside a polygon farthest from its
//! outline, where a label on the polygon has the most room.
//!
//! This is MapLibre's `findPoleOfInaccessibility` (Mapbox's `polylabel`):
//! cover the exterior ring's bounding box with square cells, then keep
//! splitting the cell whose centre could still beat the best distance so
//! far by more than `precision`, starting from the ring's centroid as the
//! first guess. Holes count as outline, so the point keeps clear of them.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::Polygon;

/// The pole of inaccessibility of `p`, found to within `precision` (in the
/// polygon's own units), rounded to the nearest integer coordinate.
///
/// A polygon whose exterior ring has no width or no height returns the
/// corner of its bounding box, as MapLibre does; one without an exterior
/// ring returns `None`.
pub fn pole_of_inaccessibility(p: &Polygon, precision: f64) -> Option<(i32, i32)> {
    let rings: Vec<&[(i32, i32)]> = std::iter::once(p.exterior.as_slice())
        .chain(p.holes.iter().map(Vec::as_slice))
        .collect();
    let (&first, rest) = p.exterior.split_first()?;
    let (mut min_x, mut min_y) = (f64::from(first.0), f64::from(first.1));
    let (mut max_x, mut max_y) = (min_x, min_y);
    for &(x, y) in rest {
        let (x, y) = (f64::from(x), f64::from(y));
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    let cell_size = (max_x - min_x).min(max_y - min_y);
    if cell_size == 0.0 {
        return Some(round(min_x, min_y));
    }

    let h = cell_size / 2.0;
    let mut queue = BinaryHeap::new();
    let mut x = min_x;
    while x < max_x {
        let mut y = min_y;
        while y < max_y {
            queue.push(Cell::new(x + h, y + h, h, &rings));
            y += cell_size;
        }
        x += cell_size;
    }

    let mut best = centroid_cell(&rings);
    while let Some(cell) = queue.pop() {
        // A zero (or, for a degenerate centroid, NaN) best distance is
        // replaced outright, as in MapLibre's `!bestCell.d`.
        if cell.d > best.d || best.d == 0.0 || best.d.is_nan() {
            best = cell;
        }
        if cell.max - best.d <= precision {
            continue;
        }
        let h = cell.h / 2.0;
        queue.push(Cell::new(cell.x - h, cell.y - h, h, &rings));
        queue.push(Cell::new(cell.x + h, cell.y - h, h, &rings));
        queue.push(Cell::new(cell.x - h, cell.y + h, h, &rings));
        queue.push(Cell::new(cell.x + h, cell.y + h, h, &rings));
    }
    Some(round(best.x, best.y))
}

fn round(x: f64, y: f64) -> (i32, i32) {
    (x.round() as i32, y.round() as i32)
}

/// A square cell: its centre, half its side, the signed distance from the
/// centre to the outline (negative outside), and the most any point in the
/// cell could reach.
#[derive(Clone, Copy)]
struct Cell {
    x: f64,
    y: f64,
    h: f64,
    d: f64,
    max: f64,
}

impl Cell {
    fn new(x: f64, y: f64, h: f64, rings: &[&[(i32, i32)]]) -> Self {
        let d = signed_distance(x, y, rings);
        Self {
            x,
            y,
            h,
            d,
            max: d + h * std::f64::consts::SQRT_2,
        }
    }
}

// The queue pops the cell with the greatest potential first.
impl Ord for Cell {
    fn cmp(&self, other: &Self) -> Ordering {
        self.max.total_cmp(&other.max)
    }
}
impl PartialOrd for Cell {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Cell {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Cell {}

/// The exterior ring's area centroid, as the first guess.
fn centroid_cell(rings: &[&[(i32, i32)]]) -> Cell {
    let ring = rings[0];
    let (mut area, mut cx, mut cy) = (0.0, 0.0, 0.0);
    let mut j = ring.len() - 1;
    for (i, &(ax, ay)) in ring.iter().enumerate() {
        let (ax, ay) = (f64::from(ax), f64::from(ay));
        let (bx, by) = (f64::from(ring[j].0), f64::from(ring[j].1));
        let f = ax * by - bx * ay;
        cx += (ax + bx) * f;
        cy += (ay + by) * f;
        area += f * 3.0;
        j = i;
    }
    Cell::new(cx / area, cy / area, 0.0, rings)
}

/// Distance from `(px, py)` to the nearest ring edge, positive inside the
/// polygon (even-odd over every ring) and negative outside. Rings may be
/// open or closed: the edge back to the start is always included.
fn signed_distance(px: f64, py: f64, rings: &[&[(i32, i32)]]) -> f64 {
    let mut inside = false;
    let mut min_sq = f64::INFINITY;
    for ring in rings {
        if ring.is_empty() {
            continue;
        }
        let mut j = ring.len() - 1;
        for (i, &(ax, ay)) in ring.iter().enumerate() {
            let (ax, ay) = (f64::from(ax), f64::from(ay));
            let (bx, by) = (f64::from(ring[j].0), f64::from(ring[j].1));
            if (ay > py) != (by > py) && px < (bx - ax) * (py - ay) / (by - ay) + ax {
                inside = !inside;
            }
            min_sq = min_sq.min(segment_distance_sq(px, py, ax, ay, bx, by));
            j = i;
        }
    }
    let d = min_sq.sqrt();
    if inside {
        d
    } else {
        -d
    }
}

fn segment_distance_sq(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let (dx, dy) = (bx - ax, by - ay);
    let l2 = dx * dx + dy * dy;
    let (qx, qy) = if l2 == 0.0 {
        (ax, ay)
    } else {
        let t = ((px - ax) * dx + (py - ay) * dy) / l2;
        if t < 0.0 {
            (ax, ay)
        } else if t > 1.0 {
            (bx, by)
        } else {
            (ax + dx * t, ay + dy * t)
        }
    };
    (px - qx) * (px - qx) + (py - qy) * (py - qy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poly(exterior: &[(i32, i32)], holes: &[&[(i32, i32)]]) -> Polygon {
        Polygon {
            exterior: exterior.to_vec(),
            holes: holes.iter().map(|h| h.to_vec()).collect(),
        }
    }

    #[test]
    fn a_square_labels_at_its_centre() {
        let p = poly(&[(0, 0), (100, 0), (100, 100), (0, 100)], &[]);
        assert_eq!(pole_of_inaccessibility(&p, 1.0), Some((50, 50)));
    }

    #[test]
    fn an_l_shape_labels_inside_its_wider_arm_not_at_its_centroid() {
        // The centroid of this L sits in the notch, outside the shape.
        let p = poly(
            &[
                (0, 0),
                (1000, 0),
                (1000, 200),
                (200, 200),
                (200, 1000),
                (0, 1000),
            ],
            &[],
        );
        let (x, y) = pole_of_inaccessibility(&p, 1.0).unwrap();
        assert!(
            signed_distance(f64::from(x), f64::from(y), &[&p.exterior]) > 90.0,
            "({x}, {y}) is not well inside the L"
        );
    }

    #[test]
    fn a_hole_pushes_the_label_off_the_centre() {
        let p = poly(
            &[(0, 0), (1000, 0), (1000, 1000), (0, 1000)],
            &[&[(300, 300), (300, 700), (700, 700), (700, 300)]],
        );
        let (x, y) = pole_of_inaccessibility(&p, 1.0).unwrap();
        let rings: Vec<&[(i32, i32)]> = vec![&p.exterior, &p.holes[0]];
        assert!(signed_distance(f64::from(x), f64::from(y), &rings) > 100.0);
    }

    #[test]
    fn closed_and_open_rings_agree() {
        let open = poly(&[(0, 0), (400, 0), (400, 100), (0, 100)], &[]);
        let closed = poly(&[(0, 0), (400, 0), (400, 100), (0, 100), (0, 0)], &[]);
        assert_eq!(
            pole_of_inaccessibility(&open, 1.0),
            pole_of_inaccessibility(&closed, 1.0)
        );
    }

    #[test]
    fn a_flat_ring_returns_its_corner() {
        let p = poly(&[(10, 20), (50, 20), (90, 20)], &[]);
        assert_eq!(pole_of_inaccessibility(&p, 1.0), Some((10, 20)));
        assert_eq!(pole_of_inaccessibility(&poly(&[], &[]), 1.0), None);
    }
}
