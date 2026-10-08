//! Junctions of a polyline network: the points where lines end or meet,
//! each with the axis a short cross tick drawn there should take.
//!
//! A network split into sections (a block's perimeter cut into frontages,
//! a road cut at its kilometre posts) shows where one section gives way to
//! the next with a tick across the line. [`junctions`] finds those places
//! from the sections' endpoints alone and picks the tick's direction from
//! the arms that meet there:
//!
//! - one arm (a line's loose end): across that arm;
//! - two arms continuing straight on: across the line;
//! - a corner: along the bisector of the angle between the arms;
//! - when the bisector would lie on or near an arm (a T-junction), or two
//!   arms close in on each other at less than [`ACUTE_CORNER_DEG`]: through
//!   the middle of the widest angular gap between the arms, so the tick
//!   never runs along a line.
//!
//! Directions are measured in tile space, x right and y down, so the axis
//! is an angle clockwise from the x axis — what a sprite rotation in
//! degrees clockwise expects.

use std::f64::consts::TAU;

/// One junction of a polyline network.
#[derive(Debug, Clone, PartialEq)]
pub struct Junction {
    /// Where the lines meet: the mean of the endpoints merged into it.
    pub point: (i32, i32),
    /// The tick's axis in degrees clockwise from the x axis, in `[0, 180)`.
    pub axis_deg: f64,
    /// How many line ends meet here.
    pub arms: usize,
}

/// Two arms closing in on each other at less than this many degrees make
/// an acute corner, whose bisector would run nearly along both lines.
pub const ACUTE_CORNER_DEG: f64 = 40.0;

/// A bisector within this many degrees of an arm lies on that arm, as at a
/// T-junction, where the arms' sum points straight down the stem.
pub const NEAR_ARM_DEG: f64 = 20.0;

/// Below this length the arms' unit vectors cancel out: the line runs
/// straight through.
const STRAIGHT_EPS: f64 = 1e-3;

/// How close, in extent units, a loose end must come to the outermost
/// vertex on its side to count as cut there by the tile's clip.
const CLIP_SLACK: i64 = 1;

/// The junctions of the polylines in `lines`.
///
/// Every polyline contributes both its endpoints, each with the direction
/// from it towards the line's next distinct vertex. Endpoints within
/// `snap` extent units of each other (chained, so the result does not
/// depend on input order) merge into one junction. A polyline with no two
/// distinct vertices is ignored.
///
/// A tile encoder cutting a line at the tile buffer leaves it a loose end
/// that is no junction at all. The end lies outside the tile square
/// `[0, extent]`, at the outermost reach of all the geometry on that side —
/// nothing passes the line the encoder cut along — and the line runs from
/// it back towards the tile. Junctions with a single arm that match all
/// three are dropped; a junction where two or more ends meet is always
/// kept, as is a loose end short of the clip line. This is the same
/// reasoning [`polygon_boundary_without_clip_edges`] applies to the edges
/// a clip adds to polygons.
///
/// A junction in the buffer of two neighbouring tiles is found the same in
/// both, with the same axis, as long as its arms reach into both buffers:
/// a clip shortens an arm without turning it.
///
/// Junctions come out in the order of the first endpoint merged into each.
///
/// [`polygon_boundary_without_clip_edges`]: super::boundary::polygon_boundary_without_clip_edges
pub fn junctions<'a, I>(lines: I, snap: f64, extent: u32) -> Vec<Junction>
where
    I: IntoIterator<Item = &'a [(i32, i32)]>,
{
    let mut ends: Vec<End> = Vec::new();
    let mut reach = Reach::default();
    for line in lines {
        reach.extend(line);
        if let Some(pair) = line_ends(line) {
            ends.extend(pair);
        }
    }
    if ends.is_empty() {
        return Vec::new();
    }

    let mut sets = DisjointSets::new(ends.len());
    let snap = snap.max(0.0);
    let snap_sq = snap * snap;
    let mut by_x: Vec<usize> = (0..ends.len()).collect();
    by_x.sort_by_key(|&i| ends[i].at);
    for (k, &i) in by_x.iter().enumerate() {
        let a = ends[i].at;
        for &j in &by_x[k + 1..] {
            let b = ends[j].at;
            let dx = f64::from(b.0) - f64::from(a.0);
            if dx > snap {
                break;
            }
            let dy = f64::from(b.1) - f64::from(a.1);
            if dx * dx + dy * dy <= snap_sq {
                sets.union(i, j);
            }
        }
    }

    // Members of each set, keyed by the set's first end in input order.
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); ends.len()];
    let mut first_of: Vec<Option<usize>> = vec![None; ends.len()];
    for i in 0..ends.len() {
        let root = sets.find(i);
        let first = *first_of[root].get_or_insert(i);
        members[first].push(i);
    }

    let extent = i64::from(extent);
    let mut out = Vec::new();
    for group in members.iter().filter(|m| !m.is_empty()) {
        if let [only] = group.as_slice() {
            if reach.is_clip_end(&ends[*only], extent) {
                continue;
            }
        }
        let n = group.len() as f64;
        let (sx, sy) = group.iter().fold((0.0, 0.0), |(x, y), &i| {
            (x + f64::from(ends[i].at.0), y + f64::from(ends[i].at.1))
        });
        let dirs: Vec<(f64, f64)> = group.iter().map(|&i| ends[i].dir).collect();
        out.push(Junction {
            point: ((sx / n).round() as i32, (sy / n).round() as i32),
            axis_deg: axis_deg(tick_direction(&dirs)),
            arms: group.len(),
        });
    }
    out
}

/// A polyline's endpoint and the unit vector from it along the line.
#[derive(Debug, Clone, Copy)]
struct End {
    at: (i32, i32),
    dir: (f64, f64),
}

fn line_ends(line: &[(i32, i32)]) -> Option<[End; 2]> {
    let first = *line.first()?;
    let last = *line.last()?;
    let next = *line.iter().find(|&&p| p != first)?;
    let prev = *line.iter().rev().find(|&&p| p != last)?;
    Some([
        End {
            at: first,
            dir: unit(first, next),
        },
        End {
            at: last,
            dir: unit(last, prev),
        },
    ])
}

fn unit(from: (i32, i32), to: (i32, i32)) -> (f64, f64) {
    let dx = f64::from(to.0) - f64::from(from.0);
    let dy = f64::from(to.1) - f64::from(from.1);
    let len = libm::hypot(dx, dy);
    (dx / len, dy / len)
}

/// The outermost vertex coordinate on each side of the input geometry.
struct Reach {
    min_x: i64,
    max_x: i64,
    min_y: i64,
    max_y: i64,
}

impl Default for Reach {
    fn default() -> Self {
        Reach {
            min_x: i64::MAX,
            max_x: i64::MIN,
            min_y: i64::MAX,
            max_y: i64::MIN,
        }
    }
}

impl Reach {
    fn extend(&mut self, line: &[(i32, i32)]) {
        for &(x, y) in line {
            let (x, y) = (i64::from(x), i64::from(y));
            self.min_x = self.min_x.min(x);
            self.max_x = self.max_x.max(x);
            self.min_y = self.min_y.min(y);
            self.max_y = self.max_y.max(y);
        }
    }

    /// `end` lies past a side of the tile, as far out as anything on that
    /// side goes, and its line runs back towards the tile.
    fn is_clip_end(&self, end: &End, extent: i64) -> bool {
        let (x, y) = (i64::from(end.at.0), i64::from(end.at.1));
        let (dx, dy) = end.dir;
        (x < 0 && x <= self.min_x + CLIP_SLACK && dx > 0.0)
            || (x > extent && x >= self.max_x - CLIP_SLACK && dx < 0.0)
            || (y < 0 && y <= self.min_y + CLIP_SLACK && dy > 0.0)
            || (y > extent && y >= self.max_y - CLIP_SLACK && dy < 0.0)
    }
}

/// The direction of the tick at a junction whose arms leave along `dirs`
/// (unit vectors, at least one).
fn tick_direction(dirs: &[(f64, f64)]) -> (f64, f64) {
    if let [only] = dirs {
        return perpendicular(*only);
    }
    let sum = dirs
        .iter()
        .fold((0.0, 0.0), |(x, y), &(dx, dy)| (x + dx, y + dy));
    let len = libm::hypot(sum.0, sum.1);
    if len < STRAIGHT_EPS {
        return match dirs {
            [a, _] => perpendicular(*a),
            _ => widest_gap_middle(dirs),
        };
    }
    let bisector = (sum.0 / len, sum.1 / len);
    let acute = matches!(dirs, [a, b] if angle_deg(*a, *b) < ACUTE_CORNER_DEG);
    let on_arm = dirs.iter().any(|&d| angle_deg(d, bisector) < NEAR_ARM_DEG);
    if acute || on_arm {
        widest_gap_middle(dirs)
    } else {
        bisector
    }
}

fn perpendicular((x, y): (f64, f64)) -> (f64, f64) {
    (-y, x)
}

/// The angle between two unit vectors, in degrees.
fn angle_deg(a: (f64, f64), b: (f64, f64)) -> f64 {
    libm::acos((a.0 * b.0 + a.1 * b.1).clamp(-1.0, 1.0)).to_degrees()
}

/// The unit vector halfway across the widest angular gap between `dirs`.
/// Of equally wide gaps, the one starting at the smallest angle wins.
fn widest_gap_middle(dirs: &[(f64, f64)]) -> (f64, f64) {
    let mut angles: Vec<f64> = dirs
        .iter()
        .map(|&(x, y)| libm::atan2(y, x).rem_euclid(TAU))
        .collect();
    angles.sort_by(f64::total_cmp);
    let mut best = (f64::NEG_INFINITY, 0.0);
    for (i, &a) in angles.iter().enumerate() {
        let next = angles.get(i + 1).copied().unwrap_or(angles[0] + TAU);
        let gap = next - a;
        if gap > best.0 {
            best = (gap, a + gap / 2.0);
        }
    }
    let mid = best.1;
    (libm::cos(mid), libm::sin(mid))
}

/// The axis of direction `d`, in degrees clockwise from the x axis in
/// `[0, 180)`, rounded to a millionth of a degree so the last bits of the
/// arithmetic do not show.
fn axis_deg((x, y): (f64, f64)) -> f64 {
    let deg = (libm::atan2(y, x).to_degrees().rem_euclid(180.0) * 1e6).round() / 1e6;
    if deg >= 180.0 {
        0.0
    } else {
        deg
    }
}

/// Union-find over end indices, with path halving.
struct DisjointSets {
    parent: Vec<usize>,
}

impl DisjointSets {
    fn new(n: usize) -> Self {
        DisjointSets {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra.max(rb)] = ra.min(rb);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::clip::{clip_line_to_rect, ClipRect, Span};

    const EXTENT: u32 = 4096;

    fn run(lines: &[Vec<(i32, i32)>], snap: f64) -> Vec<Junction> {
        junctions(lines.iter().map(Vec::as_slice), snap, EXTENT)
    }

    fn at(js: &[Junction], p: (i32, i32)) -> &Junction {
        js.iter()
            .find(|j| j.point == p)
            .unwrap_or_else(|| panic!("no junction at {p:?} in {js:?}"))
    }

    fn assert_axis(j: &Junction, deg: f64) {
        assert!(
            (j.axis_deg - deg).abs() < 1e-6,
            "axis {} at {:?}, expected {deg}",
            j.axis_deg,
            j.point
        );
    }

    #[test]
    fn endpoints_within_snap_merge_into_one_junction() {
        let js = run(
            &[
                vec![(1000, 1000), (2000, 1000)],
                vec![(2001, 1001), (3000, 1000)],
            ],
            2.0,
        );
        assert_eq!(js.len(), 3);
        let mid = &js[1];
        assert_eq!(mid.arms, 2);
        assert!((mid.point.0 - 2000).abs() <= 1 && (mid.point.1 - 1000).abs() <= 1);
    }

    #[test]
    fn endpoints_further_apart_than_snap_stay_apart() {
        let js = run(
            &[
                vec![(1000, 1000), (2000, 1000)],
                vec![(2004, 1000), (3000, 1000)],
            ],
            2.0,
        );
        assert_eq!(js.len(), 4);
        assert!(js.iter().all(|j| j.arms == 1));
    }

    #[test]
    fn merging_chains_regardless_of_input_order() {
        // Ends at x = 2000, 2001 and 2003: the outer two are 3 apart, but
        // each is within 2 of the middle one, whichever comes first.
        let a = vec![(1000, 1000), (2000, 1000)];
        let b = vec![(2003, 1000), (2003, 2000)];
        let c = vec![(2001, 1000), (3000, 1000)];
        for lines in [
            vec![a.clone(), b.clone(), c.clone()],
            vec![b.clone(), a.clone(), c.clone()],
        ] {
            let js = run(&lines, 2.0);
            assert_eq!(js.iter().filter(|j| j.arms == 3).count(), 1, "{js:?}");
        }
    }

    #[test]
    fn a_straight_continuation_ticks_across_the_line() {
        let js = run(
            &[
                vec![(1000, 1000), (2000, 1000)],
                vec![(2000, 1000), (3000, 1000)],
            ],
            1.0,
        );
        assert_axis(at(&js, (2000, 1000)), 90.0);
    }

    #[test]
    fn a_corner_ticks_along_its_bisector() {
        // Up and right from the corner: the bisector points up-right, at
        // -45° clockwise, an axis of 135°.
        let js = run(
            &[
                vec![(2000, 1000), (2000, 2000)],
                vec![(2000, 2000), (3000, 2000)],
            ],
            1.0,
        );
        let corner = at(&js, (2000, 2000));
        assert_eq!(corner.arms, 2);
        assert_axis(corner, 135.0);
    }

    #[test]
    fn a_t_junction_ticks_across_the_through_line() {
        // A stem leaving at 60° clockwise sits right on the arms' sum; the
        // widest gap, above the through line, is crossed at 270°.
        let js = run(
            &[
                vec![(1000, 2000), (2000, 2000)],
                vec![(2000, 2000), (3000, 2000)],
                vec![(2000, 2000), (2500, 2866)],
            ],
            1.0,
        );
        let t = at(&js, (2000, 2000));
        assert_eq!(t.arms, 3);
        assert_axis(t, 90.0);
    }

    #[test]
    fn an_acute_corner_ticks_through_its_wide_side() {
        // Arms at 0° and 30°: the tick crosses the 330° gap at 195°, the
        // same axis as the bisector would have but taken from the gap.
        let js = run(
            &[
                vec![(2000, 2000), (3000, 2000)],
                vec![(2000, 2000), (2866, 2500)],
            ],
            1.0,
        );
        let corner = at(&js, (2000, 2000));
        assert!((corner.axis_deg - 15.0).abs() < 0.05, "{corner:?}");
    }

    #[test]
    fn an_acute_branch_off_a_straight_line_avoids_the_branch() {
        // Through line plus a branch 25° below its right arm. The arms'
        // sum lies near the branch, so the tick takes the widest gap,
        // above the line, rather than nearly following the branch.
        let js = run(
            &[
                vec![(1000, 2000), (2000, 2000)],
                vec![(2000, 2000), (3000, 2000)],
                vec![(2000, 2000), (2906, 2423)],
            ],
            1.0,
        );
        let j = at(&js, (2000, 2000));
        assert!((j.axis_deg - 90.0).abs() < 0.05, "{j:?}");
    }

    #[test]
    fn a_loose_end_ticks_across_its_arm() {
        let js = run(&[vec![(1000, 1000), (2000, 2000)]], 1.0);
        assert_eq!(js.len(), 2);
        assert_axis(at(&js, (1000, 1000)), 135.0);
        assert_axis(at(&js, (2000, 2000)), 135.0);
    }

    #[test]
    fn the_next_distinct_vertex_sets_an_arm() {
        // A repeated first vertex does not make the arm degenerate.
        let js = run(&[vec![(1000, 1000), (1000, 1000), (1000, 2000)]], 1.0);
        assert_axis(at(&js, (1000, 1000)), 0.0);
    }

    #[test]
    fn a_degenerate_line_has_no_junctions() {
        assert!(run(&[vec![(5, 5), (5, 5)], vec![(7, 7)]], 1.0).is_empty());
    }

    #[test]
    fn ends_cut_at_the_clip_line_are_dropped() {
        let js = run(
            &[
                // Crosses the tile, cut at the left and right clip lines.
                vec![(-128, 1000), (4224, 1100)],
                // Enters from the top clip line and ends inside.
                vec![(2000, -128), (2000, 500)],
                // Ends in the buffer short of the clip line: a real end.
                vec![(-60, 3000), (800, 3000)],
                // Meets another line in the buffer: a real junction.
                vec![(-60, 2000), (500, 2000)],
                vec![(-60, 2000), (-60, 2500)],
            ],
            1.0,
        );
        let points: Vec<_> = js.iter().map(|j| j.point).collect();
        assert!(!points.contains(&(-128, 1000)), "{points:?}");
        assert!(!points.contains(&(4224, 1100)), "{points:?}");
        assert!(!points.contains(&(2000, -128)), "{points:?}");
        assert!(points.contains(&(2000, 500)));
        assert!(points.contains(&(-60, 3000)));
        assert_eq!(at(&js, (-60, 2000)).arms, 2);
    }

    #[test]
    fn an_end_on_the_clip_line_running_outward_is_kept() {
        // At the outermost reach, but its line heads further out rather
        // than back into the tile, so no clip made it.
        let js = run(
            &[
                vec![(-128, 1000), (-128, 2000)],
                vec![(-100, 3000), (500, 3000)],
            ],
            1.0,
        );
        assert!(js.iter().any(|j| j.point == (-128, 1000)));
    }

    #[test]
    fn ends_on_the_tile_border_itself_are_kept() {
        let js = run(&[vec![(0, 1000), (4096, 1000)]], 1.0);
        assert_eq!(js.len(), 2);
    }

    /// Clip `world` (in the frame of the tile at `dx` tiles right of the
    /// origin) to that tile's square plus `buffer`, as an encoder does.
    fn tile_cut(world: &[Vec<(i32, i32)>], dx: i32, buffer: i32) -> Vec<Vec<(i32, i32)>> {
        let e = EXTENT as i32;
        let r = ClipRect {
            x: Span::closed(-buffer, e + buffer),
            y: Span::closed(-buffer, e + buffer),
        };
        world
            .iter()
            .flat_map(|l| {
                let moved: Vec<_> = l.iter().map(|&(x, y)| (x - dx * e, y)).collect();
                clip_line_to_rect(&moved, &r)
            })
            .collect()
    }

    #[test]
    fn neighbouring_tiles_agree_on_a_junction_in_both_buffers() {
        let e = EXTENT as i32;
        // A frontage network straddling the seam between tile 0 and tile
        // 1, with a T-junction and a corner just either side of it.
        let world = vec![
            vec![(3000, 1000), (4060, 1000)],
            vec![(4060, 1000), (5000, 1000)],
            vec![(4060, 1000), (4100, 1800)],
            vec![(4100, 1800), (5200, 1900)],
            vec![(4100, 1800), (3500, 3000)],
            vec![(4130, 3000), (4130, 400)],
        ];
        let buffer = 128;
        let left = run(&tile_cut(&world, 0, buffer), 1.0);
        let right = run(&tile_cut(&world, 1, buffer), 1.0);
        for p in [(4060, 1000), (4100, 1800), (4130, 3000), (4130, 400)] {
            let a = at(&left, p);
            let b = at(&right, (p.0 - e, p.1));
            assert_eq!(a.arms, b.arms, "{a:?} vs {b:?}");
            assert!((a.axis_deg - b.axis_deg).abs() < 0.2, "{a:?} vs {b:?}");
        }
        // Neither tile invents a junction where its clip cut a line.
        for (js, off) in [(&left, 0), (&right, e)] {
            for j in js.iter() {
                let world_p = (j.point.0 + off, j.point.1);
                let is_real = world
                    .iter()
                    .any(|l| l.first() == Some(&world_p) || l.last() == Some(&world_p));
                assert!(is_real, "spurious junction {j:?}");
            }
        }
    }
}
