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

/// Like [`polygon_boundary`], but without the edges a tile encoder adds
/// when it clips a polygon at the tile buffer.
///
/// A segment is taken for a clip edge when both its ends lie strictly
/// outside the tile square `[0, extent]` past the same side, and it runs
/// within [`CLIP_EDGE_MAX_ANGLE_DEG`] of that side. A clip edge is exactly
/// parallel to its side, but an upstream `buffer` closing bends it a
/// little, hence the tolerance. A real outline outside the tile running
/// nearly parallel to its edge goes too; nothing outside the tile shows
/// except through a stroke bleeding back in, so this never removes a line
/// drawn inside the tile. The test is strict so a rectangle on the tile
/// border itself is kept.
///
/// Each ring is split into the runs of segments between dropped ones, one
/// polyline per run; on a closed ring a run crossing the start point comes
/// out whole. A ring with nothing dropped is returned as it is.
pub fn polygon_boundary_without_clip_edges(p: &Polygon, extent: u32) -> Vec<Vec<(i32, i32)>> {
    let clip = ClipTest {
        extent: i64::from(extent),
        max_slope: libm::tan(CLIP_EDGE_MAX_ANGLE_DEG.to_radians()),
    };
    let mut out = Vec::with_capacity(1 + p.holes.len());
    push_ring_runs(&p.exterior, &clip, &mut out);
    for h in &p.holes {
        push_ring_runs(h, &clip, &mut out);
    }
    out
}

/// How far from its tile side a clip edge may turn. Growing then shrinking
/// a polygon with `buffer` (a closing) bends the clip line where something
/// sits near it, by up to about 10°; 15° leaves a margin over that.
pub const CLIP_EDGE_MAX_ANGLE_DEG: f64 = 15.0;

struct ClipTest {
    extent: i64,
    /// `tan` of [`CLIP_EDGE_MAX_ANGLE_DEG`]: the largest cross/along ratio.
    max_slope: f64,
}

impl ClipTest {
    fn is_clip_edge(&self, a: (i32, i32), b: (i32, i32)) -> bool {
        let e = self.extent;
        let (ax, ay, bx, by) = (
            i64::from(a.0),
            i64::from(a.1),
            i64::from(b.0),
            i64::from(b.1),
        );
        let dx = (bx - ax).abs() as f64;
        let dy = (by - ay).abs() as f64;
        let past_x = (ax < 0 && bx < 0) || (ax > e && bx > e);
        let past_y = (ay < 0 && by < 0) || (ay > e && by > e);
        (past_x && dx <= dy * self.max_slope) || (past_y && dy <= dx * self.max_slope)
    }
}

fn push_ring_runs(ring: &[(i32, i32)], clip: &ClipTest, out: &mut Vec<Vec<(i32, i32)>>) {
    if ring.is_empty() {
        return;
    }
    let segs = ring.len() - 1;
    let Some(first_drop) = (0..segs).find(|&i| clip.is_clip_edge(ring[i], ring[i + 1])) else {
        out.push(ring.to_vec());
        return;
    };
    // Starting a closed ring's walk just past a dropped segment means no
    // run can straddle the walk's ends.
    let closed = ring[0] == ring[segs];
    let start = if closed { first_drop + 1 } else { 0 };
    let mut run: Vec<(i32, i32)> = Vec::new();
    for k in 0..segs {
        let i = (start + k) % segs;
        let (a, b) = (ring[i], ring[i + 1]);
        if clip.is_clip_edge(a, b) {
            flush_run(&mut run, out);
        } else {
            if run.is_empty() {
                run.push(a);
            }
            run.push(b);
        }
    }
    flush_run(&mut run, out);
}

fn flush_run(run: &mut Vec<(i32, i32)>, out: &mut Vec<Vec<(i32, i32)>>) {
    if run.len() >= 2 {
        out.push(std::mem::take(run));
    } else {
        run.clear();
    }
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

    const EXTENT: u32 = 4096;
    const COAST: [(i32, i32); 3] = [(4224, 1000), (2000, 2000), (-128, 3000)];

    #[test]
    fn clip_edges_dropped_leaving_the_coast() {
        let p = Polygon {
            exterior: vec![
                (-128, -128),
                (4224, -128),
                (4224, 1000),
                (2000, 2000),
                (-128, 3000),
                (-128, -128),
            ],
            holes: vec![],
        };
        assert_eq!(
            polygon_boundary_without_clip_edges(&p, EXTENT),
            vec![COAST.to_vec()]
        );
    }

    #[test]
    fn run_across_the_ring_start_is_one_polyline() {
        let p = Polygon {
            exterior: vec![
                (2000, 2000),
                (-128, 3000),
                (-128, -128),
                (4224, -128),
                (4224, 1000),
                (2000, 2000),
            ],
            holes: vec![],
        };
        assert_eq!(
            polygon_boundary_without_clip_edges(&p, EXTENT),
            vec![COAST.to_vec()]
        );
    }

    #[test]
    fn tile_square_on_the_border_is_kept() {
        let ring = vec![(0, 0), (4096, 0), (4096, 4096), (0, 4096), (0, 0)];
        let p = Polygon {
            exterior: ring.clone(),
            holes: vec![],
        };
        assert_eq!(polygon_boundary_without_clip_edges(&p, EXTENT), vec![ring]);
    }

    #[test]
    fn polygon_entirely_on_clip_lines_yields_nothing() {
        let p = Polygon {
            exterior: vec![
                (-128, -128),
                (4224, -128),
                (4224, 4224),
                (-128, 4224),
                (-128, -128),
            ],
            holes: vec![],
        };
        assert!(polygon_boundary_without_clip_edges(&p, EXTENT).is_empty());
    }

    #[test]
    fn holes_are_filtered_like_the_exterior() {
        let island = vec![(100, 100), (200, 100), (150, 200), (100, 100)];
        let p = Polygon {
            exterior: vec![
                (-128, -128),
                (4224, -128),
                (4224, 4224),
                (-128, 4224),
                (-128, -128),
            ],
            holes: vec![
                island.clone(),
                // An island cut by the right-hand clip line.
                vec![
                    (4000, 500),
                    (4224, 400),
                    (4224, 700),
                    (4000, 600),
                    (4000, 500),
                ],
            ],
        };
        assert_eq!(
            polygon_boundary_without_clip_edges(&p, EXTENT),
            vec![
                island,
                vec![(4224, 700), (4000, 600), (4000, 500), (4224, 400)],
            ]
        );
    }

    #[test]
    fn clip_side_bent_by_a_closing_is_dropped() {
        let p = Polygon {
            exterior: vec![
                (-128, 1184),
                (1000, 1200),
                (1000, 1400),
                (-128, 1393),
                (-123, 1363),
                (-120, 1250),
                (-128, 1184),
            ],
            holes: vec![],
        };
        assert_eq!(
            polygon_boundary_without_clip_edges(&p, EXTENT),
            vec![vec![(-128, 1184), (1000, 1200), (1000, 1400), (-128, 1393)]]
        );
    }

    #[test]
    fn steep_coast_outside_the_tile_is_kept() {
        let p = Polygon {
            exterior: vec![(4224, 100), (4324, 200), (4224, 300), (4224, 100)],
            holes: vec![],
        };
        assert_eq!(
            polygon_boundary_without_clip_edges(&p, EXTENT),
            vec![vec![(4224, 100), (4324, 200), (4224, 300)]]
        );
    }

    #[test]
    fn segment_with_ends_past_different_sides_is_kept() {
        // Across the tile, left side to right side.
        let across = Polygon {
            exterior: vec![
                (-128, 2000),
                (4224, 2100),
                (4224, 4224),
                (-128, 4224),
                (-128, 2000),
            ],
            holes: vec![],
        };
        assert_eq!(
            polygon_boundary_without_clip_edges(&across, EXTENT),
            vec![vec![(-128, 2000), (4224, 2100)]]
        );
        // Across the corner, left side to top side.
        let corner = Polygon {
            exterior: vec![(-128, 100), (100, -128), (-128, -128), (-128, 100)],
            holes: vec![],
        };
        assert_eq!(
            polygon_boundary_without_clip_edges(&corner, EXTENT),
            vec![vec![(-128, 100), (100, -128)]]
        );
    }
}
