//! Douglas-Peucker simplification via the [`geo`] crate.

use geo::{Coord, LineString, Polygon as GPolygon, Simplify};

use crate::Polygon;

use super::convert::pt_to_i;

fn line_to_geo(line: &[(i32, i32)]) -> LineString<f64> {
    LineString::from(
        line.iter()
            .map(|&(x, y)| Coord {
                x: x as f64,
                y: y as f64,
            })
            .collect::<Vec<_>>(),
    )
}

fn line_from_geo(ls: &LineString<f64>) -> Vec<(i32, i32)> {
    ls.0.iter().map(|c| pt_to_i([c.x, c.y])).collect()
}

/// Simplify a polyline. Suppresses degenerate output (returns `None`
/// when the result has fewer than two points).
pub fn simplify_line(line: &[(i32, i32)], epsilon: f64) -> Option<Vec<(i32, i32)>> {
    if line.len() < 3 {
        return Some(line.to_vec());
    }
    let simplified = line_to_geo(line).simplify(epsilon);
    let out = line_from_geo(&simplified);
    if out.len() < 2 {
        None
    } else {
        Some(out)
    }
}

/// Simplify each ring of a polygon independently. Rings collapsing to
/// fewer than 4 points are dropped (a valid closed ring needs ≥ 4
/// vertices counting the repeated endpoint).
pub fn simplify_polygon(p: &Polygon, epsilon: f64) -> Option<Polygon> {
    let exterior = LineString::from(
        p.exterior
            .iter()
            .map(|&(x, y)| Coord {
                x: x as f64,
                y: y as f64,
            })
            .collect::<Vec<_>>(),
    );
    let holes: Vec<LineString<f64>> = p
        .holes
        .iter()
        .map(|h| {
            LineString::from(
                h.iter()
                    .map(|&(x, y)| Coord {
                        x: x as f64,
                        y: y as f64,
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let g = GPolygon::new(exterior, holes).simplify(epsilon);
    let ext = line_from_geo(g.exterior());
    if ext.len() < 4 {
        return None;
    }
    let holes = g
        .interiors()
        .iter()
        .filter_map(|h| {
            let v = line_from_geo(h);
            (v.len() >= 4).then_some(v)
        })
        .collect();
    Some(Polygon {
        exterior: ext,
        holes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collinear_midpoints_are_removed() {
        let line = vec![(0, 0), (5, 0), (10, 0)];
        let simplified = simplify_line(&line, 0.1).unwrap();
        assert_eq!(simplified, vec![(0, 0), (10, 0)]);
    }
}
