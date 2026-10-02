//! Clipping geometry to an axis-aligned rectangle whose sides may be
//! open or closed.
//!
//! The open/closed distinction matters when several rectangles tile the
//! plane and each must own its share exactly once: give neighbouring
//! rectangles complementary sides (one closed, the other open) and a
//! point on the shared line lands in one of them, and a line running
//! along it is kept by one of them. Polygons don't care — a shared edge
//! has no area.

use crate::Polygon;

use super::boolean::{polygon_boolean, BoolOp};

/// One axis of a [`ClipRect`]: the interval between `lo` and `hi`, each
/// end included unless marked open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub lo: i32,
    pub hi: i32,
    pub lo_open: bool,
    pub hi_open: bool,
}

impl Span {
    /// The closed interval `[lo, hi]`.
    pub fn closed(lo: i32, hi: i32) -> Span {
        Span {
            lo,
            hi,
            lo_open: false,
            hi_open: false,
        }
    }

    pub fn contains(&self, v: i32) -> bool {
        let above = if self.lo_open {
            v > self.lo
        } else {
            v >= self.lo
        };
        let below = if self.hi_open {
            v < self.hi
        } else {
            v <= self.hi
        };
        above && below
    }

    /// Whether `v` sits on one of the open ends.
    fn on_open_end(&self, v: i32) -> bool {
        (self.lo_open && v == self.lo) || (self.hi_open && v == self.hi)
    }
}

/// An axis-aligned rectangle, `x` by `y`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipRect {
    pub x: Span,
    pub y: Span,
}

impl ClipRect {
    pub fn contains(&self, p: (i32, i32)) -> bool {
        self.x.contains(p.0) && self.y.contains(p.1)
    }
}

/// Bounding box `(x0, y0, x1, y1)` of a point run; `None` when empty.
fn bounds(pts: &[(i32, i32)]) -> Option<(i32, i32, i32, i32)> {
    let mut it = pts.iter();
    let &(x, y) = it.next()?;
    Some(it.fold((x, y, x, y), |(x0, y0, x1, y1), &(x, y)| {
        (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
    }))
}

/// The part of `p` inside `r`, as zero or more polygons.
///
/// A polygon wholly inside comes back unchanged and one wholly outside
/// (or meeting `r` only along an edge) comes back empty, both without
/// an overlay; only one that crosses a side is cut, by a boolean
/// intersection. The cutting rectangle is first shrunk to just past the
/// polygon's own bounds, so a side far out (an "unbounded" one) never
/// widens the overlay's working range and costs it precision.
pub fn clip_polygon_to_rect(p: &Polygon, r: &ClipRect) -> Vec<Polygon> {
    let Some((x0, y0, x1, y1)) = bounds(&p.exterior) else {
        return Vec::new();
    };
    if x1 <= r.x.lo || x0 >= r.x.hi || y1 <= r.y.lo || y0 >= r.y.hi {
        return Vec::new();
    }
    if x0 >= r.x.lo && x1 <= r.x.hi && y0 >= r.y.lo && y1 <= r.y.hi {
        return vec![p.clone()];
    }
    let cx0 = r.x.lo.max(x0.saturating_sub(1));
    let cx1 = r.x.hi.min(x1.saturating_add(1));
    let cy0 = r.y.lo.max(y0.saturating_sub(1));
    let cy1 = r.y.hi.min(y1.saturating_add(1));
    let rect = Polygon {
        exterior: vec![(cx0, cy0), (cx1, cy0), (cx1, cy1), (cx0, cy1)],
        holes: vec![],
    };
    polygon_boolean(std::slice::from_ref(p), &[rect], BoolOp::Intersection)
}

/// The parts of `line` inside `r`, as separate runs. A line leaving and
/// re-entering the rectangle starts a new run; a stretch running along
/// an open side is dropped, so the rectangle across that side keeps it.
/// Crossing points are rounded to the integer grid.
pub fn clip_line_to_rect(line: &[(i32, i32)], r: &ClipRect) -> Vec<Vec<(i32, i32)>> {
    if line.is_empty() {
        return Vec::new();
    }
    // Every vertex inside → the whole line is (the rectangle is convex).
    if line.iter().all(|&p| r.contains(p)) {
        return vec![line.to_vec()];
    }
    let mut out: Vec<Vec<(i32, i32)>> = Vec::new();
    for w in line.windows(2) {
        let Some((a, b)) = clip_segment(w[0], w[1], r) else {
            continue;
        };
        // A stretch along an open side belongs to the rectangle beyond it.
        if (r.x.on_open_end(a.0) && a.0 == b.0) || (r.y.on_open_end(a.1) && a.1 == b.1) {
            continue;
        }
        match out.last_mut() {
            Some(run) if run.last() == Some(&a) => run.push(b),
            _ => out.push(vec![a, b]),
        }
    }
    out
}

/// Liang–Barsky: the part of segment `a → b` inside the closed `r`,
/// rounded to the grid. `None` when nothing (or a single point the
/// segment merely grazes) is left.
fn clip_segment(a: (i32, i32), b: (i32, i32), r: &ClipRect) -> Option<((i32, i32), (i32, i32))> {
    let (ax, ay) = (a.0 as f64, a.1 as f64);
    let (dx, dy) = (b.0 as f64 - ax, b.1 as f64 - ay);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-dx, ax - r.x.lo as f64),
        (dx, r.x.hi as f64 - ax),
        (-dy, ay - r.y.lo as f64),
        (dy, r.y.hi as f64 - ay),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    if t0 > t1 {
        return None;
    }
    let at = |t: f64| -> (i32, i32) {
        if t == 0.0 {
            a
        } else if t == 1.0 {
            b
        } else {
            ((ax + dx * t).round() as i32, (ay + dy * t).round() as i32)
        }
    };
    let (p, q) = (at(t0), at(t1));
    // A degenerate input segment is kept as given; one the clip shrank
    // to a point only touched the rectangle.
    if p == q && a != b {
        return None;
    }
    Some((p, q))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: Span, y: Span) -> ClipRect {
        ClipRect { x, y }
    }

    fn square(x0: i32, y0: i32, x1: i32, y1: i32) -> Polygon {
        Polygon {
            exterior: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)],
            holes: vec![],
        }
    }

    #[test]
    fn polygon_inside_is_returned_unchanged() {
        let p = square(1, 1, 5, 5);
        let r = rect(Span::closed(0, 10), Span::closed(0, 10));
        let out = clip_polygon_to_rect(&p, &r);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].exterior, p.exterior);
    }

    #[test]
    fn polygon_touching_only_an_edge_is_dropped() {
        let p = square(10, 0, 20, 10);
        let r = rect(Span::closed(0, 10), Span::closed(0, 10));
        assert!(clip_polygon_to_rect(&p, &r).is_empty());
    }

    #[test]
    fn polygon_crossing_a_far_side_is_cut_exactly() {
        let p = square(-5, 2, 5, 8);
        let far = 1 << 30;
        let r = rect(
            Span {
                lo: 0,
                hi: far,
                lo_open: true,
                hi_open: false,
            },
            Span::closed(-far, far),
        );
        let out = clip_polygon_to_rect(&p, &r);
        assert_eq!(out.len(), 1);
        let (x0, y0, x1, y1) = bounds(&out[0].exterior).unwrap();
        assert_eq!((x0, y0, x1, y1), (0, 2, 5, 8));
    }

    #[test]
    fn line_leaving_and_reentering_splits_into_runs() {
        let r = rect(Span::closed(0, 10), Span::closed(0, 10));
        let line = [(2, 5), (20, 5), (20, 7), (2, 7)];
        let out = clip_line_to_rect(&line, &r);
        assert_eq!(out, vec![vec![(2, 5), (10, 5)], vec![(10, 7), (2, 7)]]);
    }

    #[test]
    fn line_along_an_open_side_belongs_to_the_other_rect() {
        let open = rect(
            Span {
                lo: 10,
                hi: 20,
                lo_open: true,
                hi_open: false,
            },
            Span::closed(0, 10),
        );
        let closed = rect(Span::closed(0, 10), Span::closed(0, 10));
        let line = [(10, 0), (10, 10)];
        assert!(clip_line_to_rect(&line, &open).is_empty());
        assert_eq!(clip_line_to_rect(&line, &closed), vec![line.to_vec()]);
    }

    #[test]
    fn open_and_closed_ends_split_points_once() {
        let s = Span {
            lo: 0,
            hi: 10,
            lo_open: true,
            hi_open: false,
        };
        assert!(!s.contains(0));
        assert!(s.contains(10));
        assert!(Span::closed(0, 10).contains(0));
    }
}
