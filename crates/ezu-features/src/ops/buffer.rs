//! Buffer / offset (Minkowski sum with a disk) of polygons, polylines,
//! and points via [`i_overlay`].
//!
//! - Polygons: positive distance inflates, negative erodes. A heavy
//!   negative buffer may erase or split a polygon — output is
//!   therefore a `Vec<Polygon>`.
//! - Polylines: stroked into a buffered polygon strip of width
//!   `2 * distance`. Open paths.
//! - Points: emitted as round polygons of radius `distance` (approx
//!   via i_overlay's round join).
//!
//! Negative distance for polylines and points is ignored (an open
//! path or a point has no interior to erode).

use core::f64::consts::PI;

use i_overlay::mesh::outline::offset::OutlineOffset;
use i_overlay::mesh::stroke::offset::StrokeOffset;
use i_overlay::mesh::style::{LineCap, LineJoin, OutlineStyle, StrokeStyle};

use crate::Polygon;

use super::convert::{polygon_to_f, polygons_from_shapes};

/// Join (corner) style applied at vertices of the offset path.
#[derive(Debug, Clone, Copy)]
pub enum BufferJoin {
    /// Bevel — cut corners off flat.
    Bevel,
    /// Miter — sharp corners. `min_angle_rad` is the minimum interior
    /// angle below which the join falls back to bevel.
    Miter { min_angle_rad: f64 },
    /// Round — corner approximated by an arc. `max_segment_angle_rad`
    /// controls fineness (smaller → more segments).
    Round { max_segment_angle_rad: f64 },
}

impl BufferJoin {
    fn to_i_overlay(self) -> LineJoin<f64> {
        match self {
            BufferJoin::Bevel => LineJoin::Bevel,
            BufferJoin::Miter { min_angle_rad } => LineJoin::Miter(min_angle_rad),
            BufferJoin::Round {
                max_segment_angle_rad,
            } => LineJoin::Round(max_segment_angle_rad),
        }
    }
}

/// Configuration for [`buffer_polygons`].
#[derive(Debug, Clone, Copy)]
pub struct BufferOpts {
    /// Signed offset distance (in the same units as the input
    /// coordinates). Positive → inflate, negative → erode.
    pub distance: f64,
    /// Join style at corners.
    pub join: BufferJoin,
}

impl Default for BufferOpts {
    fn default() -> Self {
        Self {
            distance: 0.0,
            join: BufferJoin::Miter {
                min_angle_rad: 5.0 * PI / 180.0,
            },
        }
    }
}

/// Inflate (positive) or erode (negative) every polygon. Heavy
/// erosion can split a polygon; the result is therefore a single flat
/// `Vec<Polygon>`.
pub fn buffer_polygons(polys: &[Polygon], opts: &BufferOpts) -> Vec<Polygon> {
    if polys.is_empty() || opts.distance == 0.0 {
        return polys.to_vec();
    }
    let style = OutlineStyle::new(opts.distance).line_join(opts.join.to_i_overlay());
    let mut out = Vec::new();
    for p in polys {
        let shape: Vec<Vec<[f64; 2]>> = polygon_to_f(p);
        let shapes = shape.outline(&style);
        out.extend(polygons_from_shapes(&shapes));
    }
    out
}

/// Stroke each polyline into a buffered polygon strip of width
/// `2 * distance.abs()`. Open paths (start ≠ end). Negative distance
/// has no meaning here and is treated as its absolute value.
pub fn buffer_lines(lines: &[Vec<(i32, i32)>], opts: &BufferOpts) -> Vec<Polygon> {
    let width = 2.0 * opts.distance.abs();
    if lines.is_empty() || width == 0.0 {
        return Vec::new();
    }
    let style = StrokeStyle::new(width)
        .line_join(opts.join.to_i_overlay())
        .start_cap(round_cap_default())
        .end_cap(round_cap_default());
    let mut out = Vec::new();
    for line in lines {
        let path: Vec<[f64; 2]> = line.iter().map(|&(x, y)| [x as f64, y as f64]).collect();
        let shapes = path.stroke(style.clone(), false);
        out.extend(polygons_from_shapes(&shapes));
    }
    out
}

/// Emit a regular n-gon approximating a disk of radius
/// `distance.abs()` per input point. Segment count is derived from
/// `BufferJoin::Round`'s `max_segment_angle_rad`; other joins default
/// to 32 segments.
pub fn buffer_points(points: &[(i32, i32)], opts: &BufferOpts) -> Vec<Polygon> {
    let radius = opts.distance.abs();
    if points.is_empty() || radius == 0.0 {
        return Vec::new();
    }
    let seg = match opts.join {
        BufferJoin::Round {
            max_segment_angle_rad,
        } if max_segment_angle_rad > 0.0 => {
            ((2.0 * PI / max_segment_angle_rad).ceil() as usize).max(8)
        }
        _ => 32,
    };
    let mut out = Vec::with_capacity(points.len());
    for &(cx, cy) in points {
        let mut ring = Vec::with_capacity(seg + 1);
        for i in 0..seg {
            let t = (i as f64) / (seg as f64) * 2.0 * PI;
            let x = cx as f64 + radius * t.cos();
            let y = cy as f64 + radius * t.sin();
            ring.push((x.round() as i32, y.round() as i32));
        }
        ring.push(ring[0]);
        out.push(Polygon {
            exterior: ring,
            holes: vec![],
        });
    }
    out
}

fn round_cap_default() -> LineCap<[f64; 2]> {
    // 0.25 rad ≈ ~14°: produces ~24 segments per full circle. Good
    // enough for tile-scale rasterisation.
    LineCap::Round(0.25)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polygon_inflate_grows() {
        let p = Polygon {
            exterior: vec![(0, 0), (10, 0), (10, 10), (0, 10), (0, 0)],
            holes: vec![],
        };
        let out = buffer_polygons(&[p], &BufferOpts {
            distance: 2.0,
            join: BufferJoin::Miter { min_angle_rad: 0.1 },
        });
        assert_eq!(out.len(), 1);
        // grew outward
        assert!(out[0].exterior.iter().any(|&(x, _)| x < 0));
    }

    #[test]
    fn polygon_heavy_erode_disappears() {
        let p = Polygon {
            exterior: vec![(0, 0), (4, 0), (4, 4), (0, 4), (0, 0)],
            holes: vec![],
        };
        let out = buffer_polygons(&[p], &BufferOpts {
            distance: -10.0,
            join: BufferJoin::Bevel,
        });
        assert!(out.is_empty());
    }

    #[test]
    fn line_buffer_produces_polygon() {
        let line = vec![(0, 0), (10, 0)];
        let out = buffer_lines(&[line], &BufferOpts {
            distance: 2.0,
            join: BufferJoin::Bevel,
        });
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn point_buffer_produces_disk() {
        let out = buffer_points(&[(5, 5)], &BufferOpts {
            distance: 3.0,
            join: BufferJoin::Bevel,
        });
        assert_eq!(out.len(), 1);
        assert!(out[0].exterior.len() >= 8);
    }
}
