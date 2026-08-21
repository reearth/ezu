//! Voronoi-based geometry ops: cell edges from a point set, polygon
//! fracture by seed sites, and a polygon's medial axis approximation.
//!
//! All three are built on top of the [`voronoice`] crate's
//! point-Voronoi via [`VoronoiBuilder`]. Sites that need to escape a
//! polygon interior (fracture, medial axis) clip against the source
//! polygon using `i_overlay`.

use std::collections::{HashMap, HashSet};

use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::single::SingleFloatOverlay;
use voronoice::{BoundingBox, Point as VPoint, Voronoi, VoronoiBuilder};

use crate::Polygon;

use super::convert::{polygon_to_f, polygons_from_shapes, pt_to_i};

/// Axis-aligned bounding rectangle used to bound Voronoi diagrams.
/// Cells extend to infinity in the general case, so a finite bbox is
/// always required.
#[derive(Debug, Clone, Copy)]
pub struct VoronoiBBox {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl VoronoiBBox {
    pub fn from_points<I: IntoIterator<Item = (i32, i32)>>(points: I) -> Option<Self> {
        let mut iter = points.into_iter();
        let first = iter.next()?;
        let mut min_x = first.0 as f64;
        let mut min_y = first.1 as f64;
        let mut max_x = min_x;
        let mut max_y = min_y;
        for (x, y) in iter {
            let xf = x as f64;
            let yf = y as f64;
            min_x = min_x.min(xf);
            min_y = min_y.min(yf);
            max_x = max_x.max(xf);
            max_y = max_y.max(yf);
        }
        Some(Self {
            min_x,
            min_y,
            max_x,
            max_y,
        })
    }

    pub fn expand(mut self, by: f64) -> Self {
        self.min_x -= by;
        self.min_y -= by;
        self.max_x += by;
        self.max_y += by;
        self
    }

    fn to_voronoice(self) -> BoundingBox {
        let cx = (self.min_x + self.max_x) * 0.5;
        let cy = (self.min_y + self.max_y) * 0.5;
        // BoundingBox::new wants positive width / height; degenerate
        // (single-point) inputs still need a valid box for voronoice.
        let w = (self.max_x - self.min_x).max(1.0);
        let h = (self.max_y - self.min_y).max(1.0);
        BoundingBox::new(VPoint { x: cx, y: cy }, w, h)
    }
}

/// Build a Voronoi diagram for `points` clipped to `bbox`. Returns
/// `None` when there are fewer than three sites — Delaunay
/// triangulation (the backbone of voronoice) is undefined below that
/// count, so the diagram would be empty anyway.
fn build_voronoi(points: &[(i32, i32)], bbox: VoronoiBBox) -> Option<Voronoi> {
    // Duplicate sites are meaningless to a Voronoi diagram — a repeated
    // site owns no area — but they make `voronoice` build a degenerate
    // cell and assert its way out of it ("Edge crosses box, intersection
    // must exist"), taking the whole render with it. Real tile geometry
    // reaches that: rings arrive with their closing vertex repeated, MVT
    // polygons carry coincident vertices, and densified boundary samples
    // round to the same integer when the spacing is fine. Dedupe first.
    //
    // No caller maps cells back to input indices, so dropping duplicates
    // here is safe for all of them.
    let mut seen = HashSet::with_capacity(points.len());
    let sites: Vec<VPoint> = points
        .iter()
        .filter(|p| seen.insert(**p))
        .map(|&(x, y)| VPoint {
            x: x as f64,
            y: y as f64,
        })
        .collect();
    if sites.len() < 3 {
        return None;
    }

    // Deduping is not enough on its own: `voronoice` (0.2.0, last released
    // in 2022) asserts rather than errors on inputs its cell clipping
    // cannot handle, and real MVT rings still reach that assertion. A
    // geometry op that returns nothing costs one layer of a tile; a panic
    // costs the whole tile, and on a tile server the whole request — so
    // contain it here rather than letting a third-party assertion decide
    // whether the map renders.
    //
    // wasm32 aborts on panic instead of unwinding, so this guard only
    // holds on native hosts.
    let bounding_box = bbox.to_voronoice();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        VoronoiBuilder::default()
            .set_sites(sites)
            .set_bounding_box(bounding_box)
            .build()
    }))
    .unwrap_or(None)
}

/// All Voronoi cell edges for `points`, clipped to `bbox` and
/// deduplicated. Each output element is a two-point polyline.
///
/// `bbox` defaults to the points' own extent if `None`.
pub fn voronoi_edges(points: &[(i32, i32)], bbox: Option<VoronoiBBox>) -> Vec<Vec<(i32, i32)>> {
    if points.len() < 2 {
        return Vec::new();
    }
    let bb = bbox
        .or_else(|| VoronoiBBox::from_points(points.iter().copied()))
        .map(|b| b.expand(1.0))
        .expect("points.len() >= 2 guarantees a bbox");
    let Some(v) = build_voronoi(points, bb) else {
        return Vec::new();
    };
    let vertices = v.vertices();
    // Each cell stores its boundary as a vertex index loop. Cells
    // share edges, so dedupe by the (min, max) index pair.
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut out = Vec::new();
    for cell in v.cells() {
        let n = cell.len();
        if n < 2 {
            continue;
        }
        for i in 0..n {
            let a = cell[i];
            let b = cell[(i + 1) % n];
            let key = if a < b { (a, b) } else { (b, a) };
            if seen.insert(key) {
                let pa = &vertices[a];
                let pb = &vertices[b];
                out.push(vec![pt_to_i([pa.x, pa.y]), pt_to_i([pb.x, pb.y])]);
            }
        }
    }
    out
}

/// Fracture `polygon` into sub-polygons using `seeds` as Voronoi
/// sites. Each Voronoi cell is intersected against `polygon`, so the
/// output never escapes the input polygon's extent.
pub fn voronoi_fracture(polygon: &Polygon, seeds: &[(i32, i32)]) -> Vec<Polygon> {
    if seeds.len() < 2 || polygon.exterior.len() < 3 {
        return Vec::new();
    }
    let Some(bbox) =
        VoronoiBBox::from_points(polygon.exterior.iter().copied()).map(|b| b.expand(2.0))
    else {
        return Vec::new();
    };
    let Some(v) = build_voronoi(seeds, bbox) else {
        return Vec::new();
    };
    let subj = polygon_to_f(polygon);
    let mut out = Vec::new();
    for cell in v.iter_cells() {
        let cell_pts: Vec<[f64; 2]> = cell.iter_vertices().map(|p| [p.x, p.y]).collect();
        if cell_pts.len() < 3 {
            continue;
        }
        // i_overlay's intersect: the cell is the clip path; the input
        // polygon is the subject. Either way works for intersection.
        let clip = vec![cell_pts];
        let result = subj.overlay(&clip, OverlayRule::Intersect, FillRule::EvenOdd);
        out.extend(polygons_from_shapes(&result));
    }
    out
}

/// Medial axis approximation of `polygon`. Densifies the boundary to
/// `densify_px` spacing, computes Voronoi of those boundary points,
/// keeps edges fully interior to the polygon, and stitches them into
/// polylines. Branch polylines shorter than `min_branch_px` are
/// dropped.
///
/// This is the **point-Voronoi approximation** of the medial axis —
/// finer `densify_px` gives a more faithful axis at the cost of
/// quadratic growth in Voronoi vertex count. For most cartographic
/// uses (river / lake centerlines, label spines), a `densify_px` of
/// ~4–8 px on tile-extent coordinates is a reasonable starting point.
pub fn medial_axis(polygon: &Polygon, densify_px: f64, min_branch_px: f64) -> Vec<Vec<(i32, i32)>> {
    if polygon.exterior.len() < 3 || densify_px <= 0.0 {
        return Vec::new();
    }
    // 1. Densify every ring (exterior + holes) into a single site list.
    let mut sites = densify_ring(&polygon.exterior, densify_px);
    for hole in &polygon.holes {
        sites.extend(densify_ring(hole, densify_px));
    }
    if sites.len() < 4 {
        return Vec::new();
    }
    // 2. Voronoi over the densified boundary. Pad the bbox so cells at
    //    the polygon's extreme edges have room to develop properly.
    let Some(bbox) =
        VoronoiBBox::from_points(sites.iter().copied()).map(|b| b.expand(densify_px * 4.0))
    else {
        return Vec::new();
    };
    let Some(v) = build_voronoi(&sites, bbox) else {
        return Vec::new();
    };

    // 3. Collect Voronoi edges whose both endpoints lie inside the
    //    polygon AND whose endpoint Voronoi vertices sit far enough
    //    from any boundary site. The latter check is essentially a
    //    "circumradius" filter: each Voronoi vertex is the
    //    circumcentre of a Delaunay triangle, so its distance to the
    //    nearest boundary site is the radius of the maximum inscribed
    //    circle at that point. Spurious edges that hug the boundary
    //    (between adjacent boundary samples) have tiny circumradii;
    //    true medial-axis vertices have circumradii ≥ half the local
    //    polygon thickness.
    let vertices = v.vertices();
    let sites = v.sites();
    let min_radius = densify_px * 0.9;
    let min_radius_sq = min_radius * min_radius;
    let nearest_site_dist_sq = |x: f64, y: f64| -> f64 {
        let mut best = f64::INFINITY;
        for s in sites {
            let dx = x - s.x;
            let dy = y - s.y;
            let d2 = dx * dx + dy * dy;
            if d2 < best {
                best = d2;
            }
        }
        best
    };
    let vertex_radius_sq: Vec<f64> = vertices
        .iter()
        .map(|p| nearest_site_dist_sq(p.x, p.y))
        .collect();

    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for cell in v.cells() {
        let n = cell.len();
        for i in 0..n {
            let a = cell[i];
            let b = cell[(i + 1) % n];
            let key = if a < b { (a, b) } else { (b, a) };
            if !seen.insert(key) {
                continue;
            }
            if vertex_radius_sq[a] < min_radius_sq || vertex_radius_sq[b] < min_radius_sq {
                continue;
            }
            let pa = &vertices[a];
            let pb = &vertices[b];
            if !point_in_polygon(polygon, pa.x, pa.y) || !point_in_polygon(polygon, pb.x, pb.y) {
                continue;
            }
            edges.push(key);
        }
    }

    // 4. Stitch into polylines and prune short branches.
    stitch_polylines(vertices, &edges, min_branch_px)
}

/// Densify a closed ring so each segment is split into pieces of at
/// most `target_px` length. Output includes the original vertices.
fn densify_ring(ring: &[(i32, i32)], target_px: f64) -> Vec<(i32, i32)> {
    if ring.len() < 2 {
        return ring.to_vec();
    }
    let mut out = Vec::new();
    let n = ring.len();
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        out.push(a);
        let dx = (b.0 - a.0) as f64;
        let dy = (b.1 - a.1) as f64;
        let len = (dx * dx + dy * dy).sqrt();
        if len < target_px {
            continue;
        }
        let steps = (len / target_px).ceil() as usize;
        for k in 1..steps {
            let t = k as f64 / steps as f64;
            let x = a.0 as f64 + dx * t;
            let y = a.1 as f64 + dy * t;
            out.push((x.round() as i32, y.round() as i32));
        }
    }
    out
}

/// Ray-casting point-in-polygon. Treats holes correctly: a point in a
/// hole counts as outside.
fn point_in_polygon(p: &Polygon, x: f64, y: f64) -> bool {
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

fn ring_contains(ring: &[(i32, i32)], x: f64, y: f64) -> bool {
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

/// Build polylines from a list of Voronoi edges. Walks from
/// degree-1 / degree-≥3 vertices outward, producing one polyline
/// per "branch". Polylines whose total length is < `min_branch_px`
/// are dropped.
fn stitch_polylines(
    vertices: &[VPoint],
    edges: &[(usize, usize)],
    min_branch_px: f64,
) -> Vec<Vec<(i32, i32)>> {
    // Adjacency: vertex idx -> list of (neighbour idx, edge idx).
    let mut adj: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();
    for (e_ix, &(a, b)) in edges.iter().enumerate() {
        adj.entry(a).or_default().push((b, e_ix));
        adj.entry(b).or_default().push((a, e_ix));
    }
    let mut used = vec![false; edges.len()];

    let mut out: Vec<Vec<(i32, i32)>> = Vec::new();
    // Start each walk from a non-degree-2 vertex (endpoint or
    // junction) so each polyline is a clean branch. Anything left
    // over after that is a closed loop and gets a separate pass.
    let mut starts: Vec<usize> = adj
        .iter()
        .filter_map(|(&v, ns)| if ns.len() != 2 { Some(v) } else { None })
        .collect();
    starts.sort();
    for &start in &starts {
        let neigh = adj.get(&start).cloned().unwrap_or_default();
        for (_, e_ix) in neigh {
            if used[e_ix] {
                continue;
            }
            if let Some(poly) = walk_branch(start, e_ix, &adj, edges, &mut used, vertices) {
                if polyline_length(&poly) >= min_branch_px {
                    out.push(poly);
                }
            }
        }
    }
    // Closed loops (every vertex degree exactly 2). Walk any
    // remaining unused edge in a cycle.
    for e_ix in 0..edges.len() {
        if used[e_ix] {
            continue;
        }
        let (a, _) = edges[e_ix];
        if let Some(poly) = walk_branch(a, e_ix, &adj, edges, &mut used, vertices) {
            if polyline_length(&poly) >= min_branch_px {
                out.push(poly);
            }
        }
    }
    out
}

fn walk_branch(
    start: usize,
    first_edge: usize,
    adj: &HashMap<usize, Vec<(usize, usize)>>,
    edges: &[(usize, usize)],
    used: &mut [bool],
    vertices: &[VPoint],
) -> Option<Vec<(i32, i32)>> {
    if used[first_edge] {
        return None;
    }
    used[first_edge] = true;
    let mut poly: Vec<(i32, i32)> = Vec::new();
    let pa = &vertices[start];
    poly.push(pt_to_i([pa.x, pa.y]));
    // Take the "other end" of the first edge.
    let (a, b) = edges[first_edge];
    let mut current = if a == start { b } else { a };
    let mut prev = start;
    loop {
        let pc = &vertices[current];
        poly.push(pt_to_i([pc.x, pc.y]));
        // Stop at junctions or endpoints. Continue through degree-2
        // pass-throughs by following the unused outgoing edge.
        let ns = adj.get(&current).map(|v| v.as_slice()).unwrap_or(&[]);
        if ns.len() != 2 {
            return Some(poly);
        }
        let next = ns.iter().find(|&&(n, e)| n != prev && !used[e]).copied();
        match next {
            Some((n, e)) => {
                used[e] = true;
                prev = current;
                current = n;
            }
            None => return Some(poly),
        }
    }
}

fn polyline_length(poly: &[(i32, i32)]) -> f64 {
    poly.windows(2)
        .map(|w| {
            let dx = (w[1].0 - w[0].0) as f64;
            let dy = (w[1].1 - w[0].1) as f64;
            (dx * dx + dy * dy).sqrt()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(min: i32, max: i32) -> Polygon {
        Polygon {
            exterior: vec![(min, min), (max, min), (max, max), (min, max)],
            holes: vec![],
        }
    }

    #[test]
    fn voronoi_edges_triangle_produces_three_inner_edges() {
        // Triangle of sites → three Voronoi edges meeting at the
        // circumcentre, plus boundary segments where the cells hit
        // the bbox.
        let pts = vec![(0, 0), (10, 0), (5, 10)];
        let edges = voronoi_edges(
            &pts,
            Some(VoronoiBBox {
                min_x: -10.0,
                min_y: -10.0,
                max_x: 20.0,
                max_y: 20.0,
            }),
        );
        assert!(
            edges.len() >= 3,
            "expected ≥ 3 Voronoi edges, got {edges:?}"
        );
    }

    #[test]
    fn voronoi_fracture_produces_one_piece_per_seed() {
        let poly = square(0, 100);
        let seeds = vec![(25, 50), (75, 50), (50, 25)];
        let pieces = voronoi_fracture(&poly, &seeds);
        assert_eq!(
            pieces.len(),
            3,
            "three seeds → three pieces, got {pieces:?}"
        );
    }

    #[test]
    fn medial_axis_of_long_rectangle_runs_along_its_length() {
        // 100×10 rectangle: the medial axis is the horizontal centre line.
        let poly = Polygon {
            exterior: vec![(0, 0), (100, 0), (100, 10), (0, 10)],
            holes: vec![],
        };
        let axis = medial_axis(&poly, 2.0, 5.0);
        assert!(
            !axis.is_empty(),
            "expected at least one medial-axis polyline: {axis:?}"
        );
        // Each polyline's y coords should hover around the centre line (y=5).
        for line in &axis {
            for &(_, y) in line {
                assert!(
                    (y - 5).abs() <= 2,
                    "medial-axis y should be near centre y=5, got {y}",
                );
            }
        }
    }
}
