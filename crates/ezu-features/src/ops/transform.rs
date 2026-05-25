//! Affine transform (translate + rotate + scale) on the crate's
//! integer-coord geometry. Rotation is in radians, applied around an
//! optional pivot before translation.

use crate::Polygon;

/// Apply `(scale, rotation_rad, translate)` around `pivot` to a single
/// point. Order: translate to origin → scale → rotate → translate back +
/// translate.
fn apply(
    p: (i32, i32),
    scale: (f64, f64),
    rot: f64,
    pivot: (f64, f64),
    tx: (f64, f64),
) -> (i32, i32) {
    let x = p.0 as f64 - pivot.0;
    let y = p.1 as f64 - pivot.1;
    let sx = x * scale.0;
    let sy = y * scale.1;
    let (sin_t, cos_t) = rot.sin_cos();
    let rx = sx * cos_t - sy * sin_t;
    let ry = sx * sin_t + sy * cos_t;
    let fx = rx + pivot.0 + tx.0;
    let fy = ry + pivot.1 + tx.1;
    (fx.round() as i32, fy.round() as i32)
}

/// Apply `(scale, rotation, translate)` to every vertex of the inputs.
/// `pivot` is the centre of rotation / scale in feature-space coords.
pub fn transform(
    points: &mut [(i32, i32)],
    lines: &mut [Vec<(i32, i32)>],
    polygons: &mut [Polygon],
    scale: (f64, f64),
    rotation_rad: f64,
    pivot: (f64, f64),
    translate: (f64, f64),
) {
    for p in points.iter_mut() {
        *p = apply(*p, scale, rotation_rad, pivot, translate);
    }
    for line in lines.iter_mut() {
        for v in line.iter_mut() {
            *v = apply(*v, scale, rotation_rad, pivot, translate);
        }
    }
    for poly in polygons.iter_mut() {
        for v in poly.exterior.iter_mut() {
            *v = apply(*v, scale, rotation_rad, pivot, translate);
        }
        for hole in poly.holes.iter_mut() {
            for v in hole.iter_mut() {
                *v = apply(*v, scale, rotation_rad, pivot, translate);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_only_shifts_all_vertices() {
        let mut points = vec![(0, 0)];
        let mut lines: Vec<Vec<(i32, i32)>> = vec![];
        let mut polys = vec![Polygon {
            exterior: vec![(0, 0), (10, 0), (10, 10), (0, 10)],
            holes: vec![],
        }];
        transform(
            &mut points,
            &mut lines,
            &mut polys,
            (1.0, 1.0),
            0.0,
            (0.0, 0.0),
            (5.0, -3.0),
        );
        assert_eq!(points[0], (5, -3));
        assert_eq!(polys[0].exterior[0], (5, -3));
        assert_eq!(polys[0].exterior[2], (15, 7));
    }

    #[test]
    fn rotate_90deg_around_origin_swaps_axes() {
        let mut points = vec![(10, 0)];
        let mut lines: Vec<Vec<(i32, i32)>> = vec![];
        let mut polys: Vec<Polygon> = vec![];
        transform(
            &mut points,
            &mut lines,
            &mut polys,
            (1.0, 1.0),
            std::f64::consts::FRAC_PI_2,
            (0.0, 0.0),
            (0.0, 0.0),
        );
        // (10, 0) rotated 90° CCW → (0, 10).
        assert_eq!(points[0], (0, 10));
    }
}
