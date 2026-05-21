//! Conversion helpers between the crate's owned `(i32, i32)` /
//! [`Polygon`] geometry and the float-based shape types used by `geo`
//! and `i_overlay`.
//!
//! The crate stores tile-local pixel coordinates as integers. Numeric
//! ops (centroid, buffer, simplify, …) work in `f64` and round back to
//! `i32` at the boundary. Helpers here are kept small and allocation-
//! explicit so call sites can see when a copy happens.

use crate::Polygon;

/// Path = open or closed polyline as float points.
pub type Path = Vec<[f64; 2]>;
/// Contour set: one outer ring + zero-or-more holes, all as float
/// points. Matches `i_overlay`'s `Shape` shape (one entry in
/// `Shapes<P>`).
pub type FloatShape = Vec<Path>;

#[inline]
pub fn pt_to_f(p: (i32, i32)) -> [f64; 2] {
    [p.0 as f64, p.1 as f64]
}

#[inline]
pub fn pt_to_i(p: [f64; 2]) -> (i32, i32) {
    (p[0].round() as i32, p[1].round() as i32)
}

pub fn line_to_f(line: &[(i32, i32)]) -> Path {
    line.iter().copied().map(pt_to_f).collect()
}

pub fn line_to_i(path: &[[f64; 2]]) -> Vec<(i32, i32)> {
    path.iter().copied().map(pt_to_i).collect()
}

/// Convert a `Polygon` to a contour set (exterior first, holes after).
/// Rings are emitted as-is; callers needing a specific winding must
/// orient beforehand.
pub fn polygon_to_f(p: &Polygon) -> FloatShape {
    let mut shape = Vec::with_capacity(1 + p.holes.len());
    shape.push(line_to_f(&p.exterior));
    for h in &p.holes {
        shape.push(line_to_f(h));
    }
    shape
}

/// Reverse of [`polygon_to_f`]. The first contour becomes the
/// exterior, subsequent contours become holes.
pub fn polygon_from_f(shape: &[Path]) -> Option<Polygon> {
    let (exterior, holes) = shape.split_first()?;
    Some(Polygon {
        exterior: line_to_i(exterior),
        holes: holes.iter().map(|h| line_to_i(h)).collect(),
    })
}

/// Flatten an `i_overlay`-style `Shapes` (one entry per output
/// polygon) into our `Vec<Polygon>`.
pub fn polygons_from_shapes(shapes: &[FloatShape]) -> Vec<Polygon> {
    shapes.iter().filter_map(|s| polygon_from_f(s)).collect()
}
