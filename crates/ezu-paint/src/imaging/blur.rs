//! Separable Gaussian blur: on RGBA8 in fixed point, and on `f32` fields.
//!
//! For images the kernel is quantised to integers summing to exactly
//! [`ONE`], and both passes accumulate in integers, so the result is the
//! same on every host however the loops are vectorised. The horizontal
//! pass keeps eight fractional bits in a `u16` intermediate so the image
//! is rounded once, at the end, rather than after each pass.
//!
//! Fields (elevations and other scalars) are blurred in `f32` by
//! [`gaussian_blur_field`], which skips missing samples rather than
//! averaging them in.
//!
//! Pixels past the edge repeat the edge pixel (clamp), so a blur near the
//! border of the canvas does not darken towards transparent. The graph
//! pads every canvas by the blur radius, so in a render the clamped
//! samples land in padding that is cropped away.

/// Fixed-point one: kernel weights sum to exactly this.
const ONE_BITS: u32 = 16;
const ONE: u32 = 1 << ONE_BITS;
/// Fractional bits the horizontal pass keeps for the vertical one.
const MID_BITS: u32 = 8;

/// Gaussian weights reaching `ceil(3σ)` pixels each side — the same
/// reach the graph pads for — normalised to sum to one (up to rounding).
/// Taps the same distance either side of the centre are bit-identical.
fn normalised_weights(sigma: f32) -> Vec<f64> {
    let radius = (3.0 * sigma).ceil() as usize;
    let sigma = f64::from(sigma);
    let two_sigma_sq = 2.0 * sigma * sigma;
    let raw: Vec<f64> = (0..=2 * radius)
        .map(|i| {
            let d = i as f64 - radius as f64;
            libm::exp(-(d * d) / two_sigma_sq)
        })
        .collect();
    let sum: f64 = raw.iter().sum();
    raw.iter().map(|w| w / sum).collect()
}

/// The fixed-point kernel: [`normalised_weights`] quantised so they sum
/// to exactly [`ONE`].
fn kernel(sigma: f32) -> Vec<u32> {
    let mut weights: Vec<u32> = normalised_weights(sigma)
        .iter()
        .map(|w| (w * f64::from(ONE)).round() as u32)
        .collect();
    let radius = weights.len() / 2;
    // Rounding leaves the total a little off ONE; the centre tap, by far
    // the largest, absorbs the difference so the blur neither brightens
    // nor darkens a flat area.
    let total: i64 = weights.iter().map(|&w| i64::from(w)).sum();
    let centre = &mut weights[radius];
    *centre = (i64::from(*centre) + i64::from(ONE) - total) as u32;
    weights
}

/// Blur straight (non-premultiplied) RGBA8: every channel, alpha
/// included, with the same kernel. `src` and `dst` are `width × height ×
/// 4` bytes, row-major. A non-positive `sigma` copies.
pub fn gaussian_blur_straight(src: &[u8], dst: &mut [u8], width: usize, height: usize, sigma: f32) {
    let len = width * height * 4;
    assert_eq!(src.len(), len, "src is not width × height RGBA8");
    assert_eq!(dst.len(), len, "dst is not width × height RGBA8");
    if sigma.is_nan() || sigma <= 0.0 || len == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let weights = kernel(sigma);
    let radius = weights.len() / 2;
    let row_len = width * 4;

    // Horizontal pass, into a `u16` buffer holding `value << MID_BITS`.
    // Each row is first widened by `radius` repeated edge pixels on both
    // sides, so every tap is a plain offset and the inner loop runs over a
    // contiguous slice.
    let mut mid = vec![0u16; len];
    let mut padded = vec![0u8; (width + 2 * radius) * 4];
    let mut acc = vec![0u32; row_len];
    for y in 0..height {
        let row = &src[y * row_len..(y + 1) * row_len];
        for x in 0..width + 2 * radius {
            let sx = x.saturating_sub(radius).min(width - 1);
            padded[x * 4..x * 4 + 4].copy_from_slice(&row[sx * 4..sx * 4 + 4]);
        }
        // The kernel is symmetric: start from the centre tap, then add each
        // pair of taps the same distance either side under one multiply.
        let centre = &padded[radius * 4..radius * 4 + row_len];
        for (a, &p) in acc.iter_mut().zip(centre) {
            *a = u32::from(p) * weights[radius];
        }
        for d in 1..=radius {
            let w = weights[radius + d];
            let left = &padded[(radius - d) * 4..(radius - d) * 4 + row_len];
            let right = &padded[(radius + d) * 4..(radius + d) * 4 + row_len];
            for ((a, &l), &r) in acc.iter_mut().zip(left).zip(right) {
                *a += (u32::from(l) + u32::from(r)) * w;
            }
        }
        // At most 255 · ONE, so after the shift at most 255 << MID_BITS.
        const ROUND: u32 = 1 << (ONE_BITS - MID_BITS - 1);
        for (m, &a) in mid[y * row_len..(y + 1) * row_len].iter_mut().zip(&acc) {
            *m = ((a + ROUND) >> (ONE_BITS - MID_BITS)) as u16;
        }
    }

    // Vertical pass. The largest sum is (255 << MID_BITS) · ONE plus the
    // rounding bias, which still fits a u32.
    const SHIFT: u32 = ONE_BITS + MID_BITS;
    const ROUND: u32 = 1 << (SHIFT - 1);
    let mid_row = |y: usize| &mid[y * row_len..(y + 1) * row_len];
    for y in 0..height {
        for (a, &p) in acc.iter_mut().zip(mid_row(y)) {
            *a = u32::from(p) * weights[radius];
        }
        for d in 1..=radius {
            let w = weights[radius + d];
            let up = mid_row(y.saturating_sub(d));
            let down = mid_row((y + d).min(height - 1));
            for ((a, &u), &v) in acc.iter_mut().zip(up).zip(down) {
                *a += (u32::from(u) + u32::from(v)) * w;
            }
        }
        for (d, &a) in dst[y * row_len..(y + 1) * row_len].iter_mut().zip(&acc) {
            *d = ((a + ROUND) >> SHIFT) as u8;
        }
    }
}

/// Blur premultiplied RGBA8. The same filter as
/// [`gaussian_blur_straight`] — blurring premultiplied values is what
/// keeps colour from bleeding out of transparent pixels — followed by
/// clamping each colour channel to its alpha, since rounding the four
/// channels separately can leave one a level above it.
pub fn gaussian_blur_premultiplied(
    src: &[u8],
    dst: &mut [u8],
    width: usize,
    height: usize,
    sigma: f32,
) {
    gaussian_blur_straight(src, dst, width, height, sigma);
    for px in dst.as_chunks_mut::<4>().0 {
        let a = px[3];
        for c in &mut px[..3] {
            *c = (*c).min(a);
        }
    }
}

/// Blur an `f32` field, `width × height` values row-major. A non-positive
/// `sigma` copies.
///
/// A sample equal to `nodata`, or NaN, is missing: it is left out of the
/// sum and the weights of the samples that remain are renormalised, so a
/// hole does not drag its neighbours towards the nodata value. A pixel
/// with no sample present anywhere in its window comes out as `nodata`
/// (NaN when there is none); one with some present takes their weighted
/// mean, so a hole narrower than the kernel fills in.
///
/// Each pass carries the weighted sum and the weight of the samples it
/// found, and the vertical pass divides one by the other, which is the
/// same as renormalising over the 2-D window.
///
/// The result is the same on every host: the weights are computed once
/// through `libm`, and each output is a run of IEEE 754 multiplies and
/// adds in a fixed tap order, which Rust never fuses or reorders. Plain
/// loops over independent pixels may vectorise without changing that.
pub fn gaussian_blur_field(
    src: &[f32],
    dst: &mut [f32],
    width: usize,
    height: usize,
    sigma: f32,
    nodata: Option<f32>,
) {
    let len = width * height;
    assert_eq!(src.len(), len, "src is not width × height");
    assert_eq!(dst.len(), len, "dst is not width × height");
    if sigma.is_nan() || sigma <= 0.0 || len == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let weights: Vec<f32> = normalised_weights(sigma)
        .iter()
        .map(|&w| w as f32)
        .collect();
    let radius = weights.len() / 2;
    let missing = |v: f32| v.is_nan() || nodata == Some(v);

    // Horizontal pass, into a weighted sum and the weight it found. Each
    // row is first widened by `radius` repeated edge samples on both sides,
    // with a missing sample entered as value 0, presence 0.
    let mut sum_h = vec![0f32; len];
    let mut weight_h = vec![0f32; len];
    let mut values = vec![0f32; width + 2 * radius];
    let mut present = vec![0f32; width + 2 * radius];
    for y in 0..height {
        let row = &src[y * width..(y + 1) * width];
        for x in 0..width + 2 * radius {
            let v = row[x.saturating_sub(radius).min(width - 1)];
            (values[x], present[x]) = if missing(v) { (0.0, 0.0) } else { (v, 1.0) };
        }
        let sum = &mut sum_h[y * width..(y + 1) * width];
        let weight = &mut weight_h[y * width..(y + 1) * width];
        for (k, &w) in weights.iter().enumerate() {
            let vs = &values[k..k + width];
            let ps = &present[k..k + width];
            for (((s, wt), &v), &p) in sum.iter_mut().zip(weight.iter_mut()).zip(vs).zip(ps) {
                *s += w * v;
                *wt += w * p;
            }
        }
    }

    // Vertical pass over the two intermediates, then the division.
    let fill = nodata.unwrap_or(f32::NAN);
    let mut sum = vec![0f32; width];
    let mut weight = vec![0f32; width];
    for y in 0..height {
        sum.fill(0.0);
        weight.fill(0.0);
        for (k, &w) in weights.iter().enumerate() {
            let sy = (y + k).saturating_sub(radius).min(height - 1);
            let ss = &sum_h[sy * width..(sy + 1) * width];
            let ws = &weight_h[sy * width..(sy + 1) * width];
            for (((s, wt), &a), &b) in sum.iter_mut().zip(weight.iter_mut()).zip(ss).zip(ws) {
                *s += w * a;
                *wt += w * b;
            }
        }
        for ((d, &s), &wt) in dst[y * width..(y + 1) * width]
            .iter_mut()
            .zip(&sum)
            .zip(&weight)
        {
            *d = if wt > 0.0 { s / wt } else { fill };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_sums_to_one_and_is_symmetric() {
        for sigma in [0.3, 1.0, 2.5, 6.0, 17.0] {
            let k = kernel(sigma);
            assert_eq!(k.iter().sum::<u32>(), ONE, "σ = {sigma}");
            assert_eq!(k.len() % 2, 1);
            let r = k.len() / 2;
            for i in 0..r {
                assert_eq!(k[i], k[k.len() - 1 - i], "σ = {sigma}, tap {i}");
            }
            assert!(k[r] >= k[r - 1], "σ = {sigma}: the centre is the peak");
        }
    }

    #[test]
    fn a_flat_image_stays_flat() {
        let (w, h) = (17, 9);
        let src: Vec<u8> = [12u8, 200, 77, 255].repeat(w * h);
        let mut dst = vec![0u8; src.len()];
        gaussian_blur_straight(&src, &mut dst, w, h, 3.0);
        assert_eq!(dst, src);
    }

    #[test]
    fn an_impulse_spreads_evenly_and_keeps_its_mass() {
        let (w, h) = (21, 21);
        let mut src = vec![0u8; w * h * 4];
        let centre = (10 * w + 10) * 4;
        src[centre..centre + 4].copy_from_slice(&[255, 255, 255, 255]);
        let mut dst = vec![0u8; src.len()];
        gaussian_blur_straight(&src, &mut dst, w, h, 1.5);
        let at = |x: usize, y: usize| dst[(y * w + x) * 4];
        // Symmetric in every direction.
        assert_eq!(at(9, 10), at(11, 10));
        assert_eq!(at(10, 9), at(10, 11));
        assert_eq!(at(9, 10), at(10, 9));
        assert!(at(10, 10) > at(11, 10) && at(11, 10) > at(12, 10));
        // The mass is kept, give or take rounding of each pixel.
        let mass: u32 = dst.as_chunks::<4>().0.iter().map(|p| u32::from(p[0])).sum();
        assert!((245..=265).contains(&mass), "mass {mass}");
    }

    #[test]
    fn premultiplied_output_stays_premultiplied() {
        let (w, h) = (16, 16);
        let src: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let a = ((i * 37) % 256) as u8;
                [a, a / 2, a / 3, a]
            })
            .collect();
        let mut dst = vec![0u8; src.len()];
        gaussian_blur_premultiplied(&src, &mut dst, w, h, 2.0);
        for px in dst.as_chunks::<4>().0 {
            assert!(px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3], "{px:?}");
        }
    }

    #[test]
    fn non_positive_sigma_copies() {
        let src: Vec<u8> = (0..4 * 4 * 4).map(|i| i as u8).collect();
        let mut dst = vec![0u8; src.len()];
        gaussian_blur_straight(&src, &mut dst, 4, 4, 0.0);
        assert_eq!(dst, src);
    }

    fn blur_field(src: &[f32], w: usize, h: usize, sigma: f32, nodata: Option<f32>) -> Vec<f32> {
        let mut dst = vec![0f32; src.len()];
        gaussian_blur_field(src, &mut dst, w, h, sigma, nodata);
        dst
    }

    #[test]
    fn a_constant_field_stays_constant() {
        let (w, h) = (19, 11);
        let src = vec![1234.5f32; w * h];
        for v in blur_field(&src, w, h, 2.5, None) {
            assert!((v - 1234.5).abs() < 1e-3, "{v}");
        }
    }

    #[test]
    fn a_field_impulse_spreads_evenly_and_keeps_its_mass() {
        let (w, h) = (21, 21);
        let mut src = vec![0f32; w * h];
        src[10 * w + 10] = 1.0;
        let dst = blur_field(&src, w, h, 1.5, None);
        let at = |x: usize, y: usize| dst[y * w + x];
        assert_eq!(at(9, 10), at(11, 10));
        assert_eq!(at(10, 9), at(10, 11));
        assert_eq!(at(9, 10), at(10, 9));
        assert_eq!(at(8, 8), at(12, 12));
        assert!(at(10, 10) > at(11, 10) && at(11, 10) > at(12, 10));
        let mass: f32 = dst.iter().sum();
        assert!((mass - 1.0).abs() < 1e-4, "mass {mass}");
    }

    #[test]
    fn a_nodata_hole_does_not_pull_its_neighbours() {
        let (w, h) = (15, 15);
        let nodata = -9999.0;
        let mut src = vec![100f32; w * h];
        for y in 6..9 {
            for x in 6..9 {
                src[y * w + x] = nodata;
            }
        }
        let dst = blur_field(&src, w, h, 2.0, Some(nodata));
        // Every pixel, the hole included, has present samples in its
        // window, and they are all 100.
        for v in dst {
            assert!((v - 100.0).abs() < 1e-3, "{v}");
        }
    }

    #[test]
    fn nan_is_left_out_without_a_nodata_value() {
        let (w, h) = (9, 9);
        let mut src = vec![7f32; w * h];
        src[4 * w + 4] = f32::NAN;
        for v in blur_field(&src, w, h, 1.0, None) {
            assert!((v - 7.0).abs() < 1e-4, "{v}");
        }
    }

    #[test]
    fn an_all_nodata_window_stays_nodata() {
        let (w, h) = (20, 20);
        let nodata = -1.0;
        let mut src = vec![nodata; w * h];
        // One present sample in the far corner, beyond the 3-px reach of
        // σ = 1 from the opposite side.
        src[(h - 1) * w + (w - 1)] = 50.0;
        let dst = blur_field(&src, w, h, 1.0, Some(nodata));
        assert_eq!(dst[0], nodata);
        assert_eq!(dst[5 * w + 5], nodata);
        assert!((dst[(h - 1) * w + (w - 1)] - 50.0).abs() < 1e-4);
        assert!((dst[(h - 2) * w + (w - 2)] - 50.0).abs() < 1e-4);

        // With no nodata value, an all-NaN window comes out NaN.
        let dst = blur_field(&[f32::NAN; 16], 4, 4, 1.0, None);
        assert!(dst.iter().all(|v| v.is_nan()));
    }

    #[test]
    fn zero_sigma_is_the_identity_on_a_field() {
        let src: Vec<f32> = (0..5 * 3).map(|i| i as f32 * 0.37 - 2.0).collect();
        assert_eq!(blur_field(&src, 5, 3, 0.0, Some(-2.0)), src);
        assert_eq!(blur_field(&src, 5, 3, -1.0, None), src);
    }
}
