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
/// four orthogonal neighbours, `step` samples away (1 for the classic
/// kernel; the pixel ratio, so a denser grid sharpens the same detail).
/// Neighbours past the edge repeat the edge sample (clamp). A fractional
/// step reads each neighbour between the two samples around it, linearly.
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
    step: f32,
) {
    let len = width * height;
    assert_eq!(src.len(), len, "src is not width × height");
    assert_eq!(dst.len(), len, "dst is not width × height");
    let missing = |v: f32| v.is_nan() || nodata == Some(v);
    let centre_gain = 1.0 + 4.0 * amount;
    // Each neighbour as the sample at or before it, the one after, and the
    // weight of the one after: `step` floored, and its fraction.
    let (whole, frac) = (step.floor() as isize, step - step.floor());
    // An index `k` whole steps plus `d` from `i`, clamped to `0..n`.
    let along = |i: usize, k: isize, d: isize, n: usize| {
        (i as isize + k * whole + d).clamp(0, n as isize - 1) as usize
    };
    for y in 0..height {
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
            // The neighbour `k` steps along x (`dx`) or y.
            let tap = |k: isize, dx: bool| {
                let pick = |d: isize| match dx {
                    true => at(along(x, k, d, width), y),
                    false => at(x, along(y, k, d, height)),
                };
                // Past the whole steps, the fraction leans outward.
                let near = pick(0);
                if frac == 0.0 {
                    return near;
                }
                near + (pick(k.signum()) - near) * frac
            };
            let neigh = tap(-1, true) + tap(1, true) + tap(-1, false) + tap(1, false);
            dst[i] = centre * centre_gain - neigh * amount;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sharpen(src: &[f32], w: usize, amount: f32, nodata: Option<f32>) -> Vec<f32> {
        let mut dst = vec![0f32; src.len()];
        laplacian_sharpen_field(src, &mut dst, w, src.len() / w, amount, nodata, 1.0);
        dst
    }

    #[test]
    fn a_step_of_two_reads_neighbours_two_samples_away() {
        // The step from the 1-sample case with every sample doubled up, as
        // a grid at pixel ratio 2 holds it: each sample sharpens to what
        // its source sample did.
        let src = [
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0,
        ];
        let mut dst = vec![0f32; src.len()];
        laplacian_sharpen_field(&src, &mut dst, 12, 1, 1.0, None, 2.0);
        assert_eq!(
            dst,
            [0.0, 0.0, 0.0, 0.0, -10.0, -10.0, 20.0, 20.0, 10.0, 10.0, 10.0, 10.0]
        );
        // A step of 1.5 reads halfway between the samples 1 and 2 away.
        let src = [0.0, 0.0, 0.0, 10.0, 10.0, 10.0];
        let mut dst = vec![0f32; src.len()];
        laplacian_sharpen_field(&src, &mut dst, 6, 1, 1.0, None, 1.5);
        assert_eq!(dst, [0.0, -5.0, -10.0, 20.0, 15.0, 10.0]);
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
