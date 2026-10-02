//! 4-neighbour Laplacian sharpen on `f32` fields.

/// Sharpen an `f32` field, `width × height` values row-major, with the
/// cross Laplacian
///
/// ```text
///      0  -k   0
///     -k 1+4k -k
///      0  -k   0
/// ```
///
/// where `k` is `amount`: each sample moves away from the mean of its
/// four orthogonal neighbours. Neighbours past the edge repeat the edge
/// sample (clamp).
///
/// A sample equal to `nodata`, or NaN, is missing. A missing neighbour
/// stands in as the centre value, so it adds nothing to the difference
/// and a hole leaves its rim as it was rather than flinging it towards
/// the nodata value. A missing centre is copied through unchanged.
///
/// The result is the same on every host: each output is the same
/// sequence of IEEE 754 adds and multiplies, in a fixed order, which
/// Rust never fuses.
pub fn laplacian_sharpen_field(
    src: &[f32],
    dst: &mut [f32],
    width: usize,
    height: usize,
    amount: f32,
    nodata: Option<f32>,
) {
    let len = width * height;
    assert_eq!(src.len(), len, "src is not width × height");
    assert_eq!(dst.len(), len, "dst is not width × height");
    let missing = |v: f32| v.is_nan() || nodata == Some(v);
    let centre_gain = 1.0 + 4.0 * amount;
    for y in 0..height {
        let up = y.saturating_sub(1);
        let down = (y + 1).min(height - 1);
        for x in 0..width {
            let i = y * width + x;
            let centre = src[i];
            if missing(centre) {
                dst[i] = centre;
                continue;
            }
            let at = |sx: usize, sy: usize| {
                let v = src[sy * width + sx];
                if missing(v) {
                    centre
                } else {
                    v
                }
            };
            let left = x.saturating_sub(1);
            let right = (x + 1).min(width - 1);
            let neigh = at(left, y) + at(right, y) + at(x, up) + at(x, down);
            dst[i] = centre * centre_gain - neigh * amount;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sharpen(src: &[f32], w: usize, amount: f32, nodata: Option<f32>) -> Vec<f32> {
        let mut dst = vec![0f32; src.len()];
        laplacian_sharpen_field(src, &mut dst, w, src.len() / w, amount, nodata);
        dst
    }

    #[test]
    fn a_constant_field_is_unchanged() {
        let src = vec![812.5f32; 6 * 4];
        assert_eq!(sharpen(&src, 6, 1.0, None), src);
    }

    #[test]
    fn a_step_overshoots_on_both_sides() {
        let src = [0.0, 0.0, 0.0, 10.0, 10.0, 10.0];
        let out = sharpen(&src, 6, 1.0, None);
        assert_eq!(out, [0.0, 0.0, -10.0, 20.0, 10.0, 10.0]);
    }

    #[test]
    fn missing_neighbours_stand_in_as_the_centre() {
        let nodata = -9999.0;
        let src = [5.0, nodata, 5.0];
        assert_eq!(sharpen(&src, 3, 2.0, Some(nodata)), [5.0, nodata, 5.0]);
        let src = [5.0, f32::NAN, 5.0];
        let out = sharpen(&src, 3, 2.0, None);
        assert_eq!((out[0], out[2]), (5.0, 5.0));
        assert!(out[1].is_nan());
    }
}
