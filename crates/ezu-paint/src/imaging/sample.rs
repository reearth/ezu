//! Bilinear sampling of `f32` grids at a sub-pixel position.
//!
//! A position is given as a whole pixel plus an `f32` offset from it,
//! rather than as one canvas coordinate. Splitting the offset into whole
//! and fractional parts on its own keeps the weights independent of where
//! the pixel sits, so two tiles that overlap compute the same bytes for
//! the same world pixel even though it has a different canvas coordinate
//! in each.
//!
//! Positions past the edge read the edge sample (clamp). The graph pads
//! every canvas by the reach of the op that samples it, so in a render
//! the clamped reads land in padding that is cropped away.

/// The four grid points around a sub-pixel position, and how far the
/// position lies between them.
#[derive(Debug, Clone, Copy)]
pub struct Bilinear {
    pub x0: usize,
    pub x1: usize,
    pub y0: usize,
    pub y1: usize,
    /// Fraction of the way from `x0` to `x1`, in `[0, 1)`.
    pub tx: f32,
    /// Fraction of the way from `y0` to `y1`, in `[0, 1)`.
    pub ty: f32,
}

impl Bilinear {
    /// The taps for pixel `base` plus `offset`, on a `width × height`
    /// grid (both non-zero), with the indices clamped to the grid.
    #[inline]
    pub fn new(width: usize, height: usize, base: (i64, i64), offset: (f32, f32)) -> Self {
        let (ox, oy) = offset;
        let fx = ox.floor();
        let fy = oy.floor();
        let ix = base.0 + fx as i64;
        let iy = base.1 + fy as i64;
        let max_x = width as i64 - 1;
        let max_y = height as i64 - 1;
        Bilinear {
            x0: ix.clamp(0, max_x) as usize,
            x1: (ix + 1).clamp(0, max_x) as usize,
            y0: iy.clamp(0, max_y) as usize,
            y1: (iy + 1).clamp(0, max_y) as usize,
            tx: ox - fx,
            ty: oy - fy,
        }
    }

    /// Blend the values at the four taps, `v00` at `(x0, y0)` through
    /// `v11` at `(x1, y1)`: along x on both rows, then along y. Left
    /// unrounded, for callers that average or keep floats.
    #[inline]
    pub fn lerp(&self, v00: f32, v10: f32, v01: f32, v11: f32) -> f32 {
        let a = v00 + (v10 - v00) * self.tx;
        let b = v01 + (v11 - v01) * self.tx;
        a + (b - a) * self.ty
    }
}

/// Bilinear sample of a `width × height` row-major field at pixel `base`
/// plus `offset`, leaving out missing samples.
///
/// A sample equal to `nodata`, or NaN, is missing. When all four taps
/// are present this is [`Bilinear::lerp`]. Otherwise the missing taps
/// are dropped and the weights of the rest renormalised, so a hole does
/// not drag the value towards the nodata value; `None` when no present
/// tap carries any weight — all four missing, or the position sitting
/// exactly on missing ones.
///
/// The result is the same on every host: a fixed sequence of IEEE 754
/// multiplies, adds and one division, which Rust never fuses.
#[inline]
pub fn sample_field(
    values: &[f32],
    width: usize,
    height: usize,
    base: (i64, i64),
    offset: (f32, f32),
    nodata: Option<f32>,
) -> Option<f32> {
    let b = Bilinear::new(width, height, base, offset);
    let v = [
        values[b.y0 * width + b.x0],
        values[b.y0 * width + b.x1],
        values[b.y1 * width + b.x0],
        values[b.y1 * width + b.x1],
    ];
    let present = |v: f32| !v.is_nan() && nodata != Some(v);
    if v.iter().all(|&s| present(s)) {
        return Some(b.lerp(v[0], v[1], v[2], v[3]));
    }
    let (sx, sy) = (1.0 - b.tx, 1.0 - b.ty);
    let weights = [sx * sy, b.tx * sy, sx * b.ty, b.tx * b.ty];
    let mut sum = 0.0f32;
    let mut weight = 0.0f32;
    for (&s, &w) in v.iter().zip(&weights) {
        if present(s) {
            sum += w * s;
            weight += w;
        }
    }
    (weight > 0.0).then(|| sum / weight)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taps_clamp_at_the_edges() {
        let b = Bilinear::new(4, 3, (0, 0), (-2.5, -0.25));
        assert_eq!((b.x0, b.x1, b.y0, b.y1), (0, 0, 0, 0));
        assert_eq!((b.tx, b.ty), (0.5, 0.75));
        let b = Bilinear::new(4, 3, (3, 2), (0.5, 0.5));
        assert_eq!((b.x0, b.x1, b.y0, b.y1), (3, 3, 2, 2));
    }

    #[test]
    fn the_weights_do_not_depend_on_the_base_pixel() {
        let a = Bilinear::new(100, 100, (10, 20), (1.3, -0.7));
        let b = Bilinear::new(100, 100, (60, 70), (1.3, -0.7));
        assert_eq!((a.tx, a.ty), (b.tx, b.ty));
        assert_eq!((a.x0 + 50, a.y0 + 50), (b.x0, b.y0));
    }

    #[test]
    fn the_taps_interpolate() {
        // 0 1
        // 2 3
        let at = |ox, oy| Bilinear::new(2, 2, (0, 0), (ox, oy)).lerp(0.0, 1.0, 2.0, 3.0);
        assert_eq!(at(0.0, 0.0), 0.0);
        assert_eq!(at(0.5, 0.0), 0.5);
        assert_eq!(at(0.0, 0.5), 1.0);
        assert_eq!(at(0.5, 0.5), 1.5);
    }

    #[test]
    fn a_full_field_sample_is_the_plain_blend() {
        let values = [0.0, 1.0, 2.0, 3.0];
        let b = Bilinear::new(2, 2, (0, 0), (0.25, 0.75));
        assert_eq!(
            sample_field(&values, 2, 2, (0, 0), (0.25, 0.75), Some(-1.0)),
            Some(b.lerp(0.0, 1.0, 2.0, 3.0))
        );
    }

    #[test]
    fn a_missing_tap_is_left_out_and_the_rest_renormalised() {
        let nodata = -9999.0;
        let values = [10.0, nodata, 10.0, 10.0];
        let v = sample_field(&values, 2, 2, (0, 0), (0.5, 0.5), Some(nodata)).unwrap();
        assert!((v - 10.0).abs() < 1e-5, "{v}");
        let values = [10.0, f32::NAN, 10.0, 10.0];
        let v = sample_field(&values, 2, 2, (0, 0), (0.5, 0.5), None).unwrap();
        assert!((v - 10.0).abs() < 1e-5, "{v}");
    }

    #[test]
    fn no_present_weight_is_missing() {
        let nodata = -1.0;
        assert_eq!(
            sample_field(&[nodata; 4], 2, 2, (0, 0), (0.5, 0.5), Some(nodata)),
            None
        );
        // Sitting exactly on the one missing tap: the others have no
        // weight, so there is nothing to renormalise.
        let values = [nodata, 5.0, 5.0, 5.0];
        assert_eq!(
            sample_field(&values, 2, 2, (0, 0), (0.0, 0.0), Some(nodata)),
            None
        );
    }
}
