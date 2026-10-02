//! Shared helpers for the gradient nodes (hillshade, slope, flow-field).

use ezu_graph::ScalarField;

/// Horn (1981) 3×3 weighted central differences. Returns
/// `(dz/dx, dz/dy)` in metres-per-metre once `inv_x` / `inv_y` carry
/// the `1 / (8 * pitch)` factor.
///
/// A sample equal to the field's `nodata`, or NaN, is missing. A
/// missing neighbour stands in as the centre value, as in `sharpen`, so
/// the edge of a hole reads as locally flat rather than as a cliff down
/// to the nodata value. A missing centre has no gradient: `None`, and
/// each caller decides what it draws there. The stand-in only swaps a
/// missing sample for the centre, so a window with nothing missing goes
/// through exactly the arithmetic it always did.
#[inline]
pub(super) fn horn_gradient(
    field: &ScalarField,
    x: u32,
    y: u32,
    inv_x: f32,
    inv_y: f32,
) -> Option<(f32, f32)> {
    let w = field.width;
    let h = field.height;
    let missing = |v: f32| v.is_nan() || field.nodata == Some(v);
    let at = |xx: u32, yy: u32| -> f32 { field.values[(yy * w + xx) as usize] };
    let e = at(x, y);
    if missing(e) {
        return None;
    }
    let xm = x.saturating_sub(1);
    let ym = y.saturating_sub(1);
    let xp = (x + 1).min(w - 1);
    let yp = (y + 1).min(h - 1);
    let z = |xx: u32, yy: u32| -> f32 {
        let v = at(xx, yy);
        if missing(v) {
            e
        } else {
            v
        }
    };
    let a = z(xm, ym);
    let b = z(x, ym);
    let c = z(xp, ym);
    let d = z(xm, y);
    let f = z(xp, y);
    let g = z(xm, yp);
    let h_ = z(x, yp);
    let i_ = z(xp, yp);
    let dz_dx = ((c + 2.0 * f + i_) - (a + 2.0 * d + g)) * inv_x;
    let dz_dy = ((g + 2.0 * h_ + i_) - (a + 2.0 * b + c)) * inv_y;
    Some((dz_dx, dz_dy))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gradient as it was before missing samples were handled.
    fn before(field: &ScalarField, x: u32, y: u32, inv_x: f32, inv_y: f32) -> (f32, f32) {
        let w = field.width;
        let h = field.height;
        let xm = x.saturating_sub(1);
        let ym = y.saturating_sub(1);
        let xp = (x + 1).min(w - 1);
        let yp = (y + 1).min(h - 1);
        let z = |xx: u32, yy: u32| -> f32 { field.values[(yy * w + xx) as usize] };
        let a = z(xm, ym);
        let b = z(x, ym);
        let c = z(xp, ym);
        let d = z(xm, y);
        let f = z(xp, y);
        let g = z(xm, yp);
        let h_ = z(x, yp);
        let i_ = z(xp, yp);
        let dz_dx = ((c + 2.0 * f + i_) - (a + 2.0 * d + g)) * inv_x;
        let dz_dy = ((g + 2.0 * h_ + i_) - (a + 2.0 * b + c)) * inv_y;
        (dz_dx, dz_dy)
    }

    fn field(nodata: Option<f32>, values: Vec<f32>, w: u32) -> ScalarField {
        ScalarField {
            width: w,
            height: values.len() as u32 / w,
            values: values.into(),
            nodata,
            geo_scale: None,
        }
    }

    #[test]
    fn a_field_with_nothing_missing_is_unchanged() {
        let (w, h) = (9u32, 7u32);
        let values: Vec<f32> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                300.0 + 41.3 * libm::sinf(0.7 * x) - 17.9 * y + 0.37 * x * y
            })
            .collect();
        for nodata in [None, Some(-9999.0)] {
            let field = field(nodata, values.clone(), w);
            for y in 0..h {
                for x in 0..w {
                    let (inv_x, inv_y) = (1.0 / 240.0, 1.0 / 216.0);
                    let (dx, dy) = horn_gradient(&field, x, y, inv_x, inv_y).unwrap();
                    let (bx, by) = before(&field, x, y, inv_x, inv_y);
                    assert_eq!((dx.to_bits(), dy.to_bits()), (bx.to_bits(), by.to_bits()));
                }
            }
        }
    }

    #[test]
    fn missing_neighbours_stand_in_as_the_centre() {
        // A level plane at 5 around a hole: flat right up to its edge.
        let nodata = -9999.0;
        #[rustfmt::skip]
        let values = vec![
            5.0, 5.0, 5.0,
            5.0, 5.0, nodata,
            5.0, f32::NAN, 5.0,
        ];
        let field = field(Some(nodata), values, 3);
        assert_eq!(horn_gradient(&field, 1, 1, 1.0, 1.0), Some((0.0, 0.0)));
        assert_eq!(horn_gradient(&field, 2, 1, 1.0, 1.0), None);
        assert_eq!(horn_gradient(&field, 1, 2, 1.0, 1.0), None);
    }
}
