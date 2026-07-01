//! Pixel-comparison metrics for `ezu-compare`.
//!
//! Kept free of any rendering/IO so it is trivially unit-testable: it
//! operates on two equal-sized RGBA8 buffers and produces both scalar
//! metrics and a diff image.

/// Scalar similarity metrics between two RGBA8 images of the same size.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub width: u32,
    pub height: u32,
    /// Root-mean-square error over RGB channels, 0..255 (alpha excluded —
    /// both renders are opaque tiles and alpha noise would distort it).
    pub rmse: f64,
    /// Mean absolute per-channel difference over RGB, 0..255.
    pub mae: f64,
    /// Fraction (0..1) of pixels whose max RGB channel difference exceeds
    /// `threshold` — the "visibly different" pixel share.
    pub diff_fraction: f64,
    /// Largest single-channel absolute difference seen, 0..255.
    pub max_diff: u8,
    /// Threshold used for `diff_fraction`.
    pub threshold: u8,
    /// Mean structural similarity (SSIM) over 8×8 luma windows, `-1..=1`
    /// (1 = identical). Structural rather than per-pixel, so it isn't
    /// dominated by antialiasing / label differences the way RMSE is.
    pub ssim: f64,
}

impl Metrics {
    /// A rough 0..100 "closeness" score derived from RMSE (100 = identical).
    /// Convenience for at-a-glance tables; RMSE/diff_fraction are the real
    /// numbers.
    pub fn score(&self) -> f64 {
        (100.0 * (1.0 - self.rmse / 255.0)).clamp(0.0, 100.0)
    }
}

/// Compare two RGBA8 buffers (`w*h*4` bytes each). `threshold` is the
/// per-channel difference above which a pixel counts as "visibly changed".
///
/// Returns `None` if the buffers don't match the given dimensions.
pub fn compare_rgba8(
    a: &[u8],
    b: &[u8],
    width: u32,
    height: u32,
    threshold: u8,
) -> Option<Metrics> {
    let n = (width as usize) * (height as usize) * 4;
    if a.len() != n || b.len() != n {
        return None;
    }
    let mut sq_sum = 0f64;
    let mut abs_sum = 0f64;
    let mut diff_pixels = 0u64;
    let mut max_diff = 0u8;
    let px = (width as usize) * (height as usize);
    for i in 0..px {
        let o = i * 4;
        let mut pixel_max = 0u8;
        for c in 0..3 {
            let da = a[o + c] as i32;
            let db = b[o + c] as i32;
            let d = (da - db).unsigned_abs() as u8;
            sq_sum += (d as f64) * (d as f64);
            abs_sum += d as f64;
            pixel_max = pixel_max.max(d);
        }
        max_diff = max_diff.max(pixel_max);
        if pixel_max > threshold {
            diff_pixels += 1;
        }
    }
    let channels = (px * 3) as f64;
    Some(Metrics {
        width,
        height,
        rmse: (sq_sum / channels).sqrt(),
        mae: abs_sum / channels,
        diff_fraction: diff_pixels as f64 / px as f64,
        max_diff,
        threshold,
        ssim: ssim_gray(a, b, width, height),
    })
}

/// Mean SSIM over non-overlapping 8×8 luma windows. Returns `1.0` for
/// identical inputs; degrades gracefully at edge windows smaller than 8×8.
pub fn ssim_gray(a: &[u8], b: &[u8], width: u32, height: u32) -> f64 {
    const WIN: usize = 8;
    // ITU-R BT.601 luma, matching `pick-channel`'s convention.
    let luma = |px: &[u8]| 0.299 * px[0] as f64 + 0.587 * px[1] as f64 + 0.114 * px[2] as f64;
    let (w, h) = (width as usize, height as usize);
    let c1 = (0.01 * 255.0f64).powi(2);
    let c2 = (0.03 * 255.0f64).powi(2);

    let mut sum = 0.0;
    let mut windows = 0.0;
    let mut wy = 0;
    while wy < h {
        let mut wx = 0;
        while wx < w {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab, mut n) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
            for y in wy..(wy + WIN).min(h) {
                for x in wx..(wx + WIN).min(w) {
                    let o = (y * w + x) * 4;
                    let (va, vb) = (luma(&a[o..o + 4]), luma(&b[o..o + 4]));
                    sa += va;
                    sb += vb;
                    saa += va * va;
                    sbb += vb * vb;
                    sab += va * vb;
                    n += 1.0;
                }
            }
            let (ma, mb) = (sa / n, sb / n);
            let va = saa / n - ma * ma;
            let vb = sbb / n - mb * mb;
            let cov = sab / n - ma * mb;
            sum += ((2.0 * ma * mb + c1) * (2.0 * cov + c2))
                / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            windows += 1.0;
            wx += WIN;
        }
        wy += WIN;
    }
    if windows == 0.0 {
        1.0
    } else {
        sum / windows
    }
}

/// Build an RGBA8 diff image: each pixel is the absolute per-channel RGB
/// difference (amplified by `gain`), opaque. Bright = large disagreement,
/// black = identical. Same dimensions as the inputs.
pub fn diff_image(a: &[u8], b: &[u8], width: u32, height: u32, gain: f32) -> Vec<u8> {
    let px = (width as usize) * (height as usize);
    let mut out = vec![0u8; px * 4];
    for i in 0..px {
        let o = i * 4;
        for c in 0..3 {
            let d = (a[o + c] as i32 - b[o + c] as i32).unsigned_abs() as f32;
            out[o + c] = (d * gain).min(255.0) as u8;
        }
        out[o + 3] = 255;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_images_score_perfect() {
        let a = vec![128u8; 4 * 4 * 4];
        let m = compare_rgba8(&a, &a, 4, 4, 8).unwrap();
        assert_eq!(m.rmse, 0.0);
        assert_eq!(m.diff_fraction, 0.0);
        assert_eq!(m.max_diff, 0);
        assert_eq!(m.score(), 100.0);
    }

    #[test]
    fn counts_visibly_different_pixels() {
        // 2x1 image: one identical pixel, one fully different (R 0 vs 255).
        let a = [0u8, 0, 0, 255, 0, 0, 0, 255];
        let b = [0u8, 0, 0, 255, 255, 0, 0, 255];
        let m = compare_rgba8(&a, &b, 2, 1, 8).unwrap();
        assert_eq!(m.max_diff, 255);
        assert_eq!(m.diff_fraction, 0.5);
        let d = diff_image(&a, &b, 2, 1, 1.0);
        assert_eq!(&d[0..4], &[0, 0, 0, 255]); // identical pixel → black
        assert_eq!(&d[4..8], &[255, 0, 0, 255]); // R differs fully
    }

    #[test]
    fn rejects_size_mismatch() {
        assert!(compare_rgba8(&[0; 4], &[0; 8], 1, 1, 8).is_none());
    }

    #[test]
    fn ssim_identical_is_one_and_differs_when_different() {
        // 8×8 mid-grey vs itself → SSIM 1.0.
        let a = vec![128u8; 8 * 8 * 4];
        let m = compare_rgba8(&a, &a, 8, 8, 8).unwrap();
        assert!((m.ssim - 1.0).abs() < 1e-9, "ssim {}", m.ssim);

        // Half black / half white → structurally very different from grey.
        let mut b = vec![0u8; 8 * 8 * 4];
        for i in 0..64 {
            let v = if i % 8 < 4 { 0 } else { 255 };
            b[i * 4] = v;
            b[i * 4 + 1] = v;
            b[i * 4 + 2] = v;
            b[i * 4 + 3] = 255;
        }
        let m2 = compare_rgba8(&a, &b, 8, 8, 8).unwrap();
        assert!(m2.ssim < 0.5, "expected low ssim, got {}", m2.ssim);
    }
}
