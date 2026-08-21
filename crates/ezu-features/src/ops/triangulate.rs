//! Delaunay triangulation of a point set. Each triangle is emitted
//! as a closed [`Polygon`] (3-vertex exterior, no holes). Useful for
//! mesh-style stylisation or as a building block for a Voronoi
//! companion.

use delaunator::{triangulate as delaunay, Point as DPoint};

use crate::Polygon;

/// Compute the Delaunay triangulation of `points` and emit each
/// triangle as a polygon. Returns an empty vec when there are fewer
/// than 3 distinct sites — Delaunay is undefined below that.
pub fn triangulate(points: &[(i32, i32)]) -> Vec<Polygon> {
    if points.len() < 3 {
        return Vec::new();
    }
    let sites: Vec<DPoint> = points
        .iter()
        .map(|&(x, y)| DPoint {
            x: x as f64,
            y: y as f64,
        })
        .collect();
    let tri = delaunay(&sites);
    // `triangles` is a flat triple-of-indices array — every 3
    // consecutive entries form a CCW Delaunay triangle.
    let (triangles, _) = tri.triangles.as_chunks::<3>();
    triangles
        .iter()
        .filter_map(|&[i, j, k]| {
            let a = points.get(i)?;
            let b = points.get(j)?;
            let c = points.get(k)?;
            Some(Polygon {
                exterior: vec![*a, *b, *c],
                holes: vec![],
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triangulate_quad_gives_two_triangles() {
        // 4 corners of a square → 2 Delaunay triangles.
        let pts = vec![(0, 0), (10, 0), (10, 10), (0, 10)];
        let tris = triangulate(&pts);
        assert_eq!(tris.len(), 2, "got {tris:?}");
    }

    #[test]
    fn triangulate_under_three_returns_empty() {
        assert!(triangulate(&[(0, 0), (1, 0)]).is_empty());
    }
}
