//! Morphological min / max over a square window, on `f32` fields.
//!
//! The filter is separable: the minimum (or maximum) over a square is
//! the minimum over the row minima, so a horizontal pass and a vertical
//! one give the 2-D result. The window is cut off at the field's edge,
//! which for a min or max is the same as repeating the edge sample.

/// Which end of the window a filter keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extremum {
    /// Erosion: valleys widen, peaks shrink.
    Min,
    /// Dilation: peaks widen, valleys shrink.
    Max,
}

/// Take the minimum or maximum over the `(2·radius + 1)²` square around
/// each sample of an `f32` field, `width × height` values row-major. A
/// zero `radius` copies.
///
/// A sample equal to `nodata`, or NaN, is missing and left out. A window
/// with no sample present comes out as `nodata` (NaN when there is
/// none); one with some present takes their extremum, so a hole
/// narrower than the window fills in from its rim.
///
/// The result is the same on every host: a comparison picks one of its
/// inputs unchanged, so no rounding enters at all.
pub fn extremum_filter_field(
    src: &[f32],
    dst: &mut [f32],
    width: usize,
    height: usize,
    radius: usize,
    extremum: Extremum,
    nodata: Option<f32>,
) {
    let len = width * height;
    assert_eq!(src.len(), len, "src is not width × height");
    assert_eq!(dst.len(), len, "dst is not width × height");
    if radius == 0 || len == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let pick = |a: f32, b: f32| match extremum {
        Extremum::Min => a.min(b),
        Extremum::Max => a.max(b),
    };
    // The extremum of the present samples in `window`, or NaN when there
    // are none. NaN marks a missing sample in the intermediate too.
    let reduce = |window: &mut dyn Iterator<Item = f32>| {
        window
            .filter(|&v| !v.is_nan() && nodata != Some(v))
            .reduce(pick)
            .unwrap_or(f32::NAN)
    };

    let mut mid = vec![0f32; len];
    for y in 0..height {
        let row = &src[y * width..(y + 1) * width];
        for x in 0..width {
            let span = x.saturating_sub(radius)..(x + radius + 1).min(width);
            mid[y * width + x] = reduce(&mut row[span].iter().copied());
        }
    }
    let fill = nodata.unwrap_or(f32::NAN);
    for y in 0..height {
        let span = y.saturating_sub(radius)..(y + radius + 1).min(height);
        for x in 0..width {
            let v = reduce(&mut span.clone().map(|sy| mid[sy * width + x]));
            dst[y * width + x] = if v.is_nan() { fill } else { v };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(src: &[f32], w: usize, r: usize, e: Extremum, nodata: Option<f32>) -> Vec<f32> {
        let mut dst = vec![0f32; src.len()];
        extremum_filter_field(src, &mut dst, w, src.len() / w, r, e, nodata);
        dst
    }

    #[test]
    fn a_square_window_spreads_a_peak() {
        let (w, h) = (7, 7);
        let mut src = vec![0f32; w * h];
        src[3 * w + 3] = 5.0;
        let grown = filter(&src, w, 1, Extremum::Max, None);
        for y in 0..h {
            for x in 0..w {
                let inside = (2..=4).contains(&x) && (2..=4).contains(&y);
                assert_eq!(
                    grown[y * w + x],
                    if inside { 5.0 } else { 0.0 },
                    "({x}, {y})"
                );
            }
        }
        assert!(filter(&src, w, 1, Extremum::Min, None)
            .iter()
            .all(|&v| v == 0.0));
    }

    #[test]
    fn missing_samples_are_left_out() {
        let nodata = -9999.0;
        // 1 2 3 4 5 with the middle missing.
        let src = [1.0, 2.0, nodata, 4.0, 5.0];
        assert_eq!(
            filter(&src, 5, 1, Extremum::Min, Some(nodata)),
            [1.0, 1.0, 2.0, 4.0, 4.0]
        );
        let src = [1.0, 2.0, f32::NAN, 4.0, 5.0];
        assert_eq!(
            filter(&src, 5, 1, Extremum::Max, None),
            [2.0, 2.0, 4.0, 5.0, 5.0]
        );
    }

    #[test]
    fn an_all_missing_window_stays_missing() {
        let nodata = -1.0;
        let src = [nodata, nodata, nodata, nodata, 7.0];
        assert_eq!(
            filter(&src, 5, 1, Extremum::Max, Some(nodata)),
            [nodata, nodata, nodata, 7.0, 7.0]
        );
        let out = filter(&[f32::NAN; 4], 2, 1, Extremum::Min, None);
        assert!(out.iter().all(|v| v.is_nan()));
    }

    #[test]
    fn zero_radius_copies() {
        let src = [3.0, -1.0, 2.0, 0.5];
        assert_eq!(filter(&src, 2, 0, Extremum::Min, None), src);
    }
}
