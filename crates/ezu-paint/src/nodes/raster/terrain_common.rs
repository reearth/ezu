//! Shared helpers for ScalarField-consuming nodes (hillshade, slope,
//! color-ramp).

use ezu_graph::ScalarField;

/// Horn (1981) 3×3 weighted central differences. Returns
/// `(dz/dx, dz/dy)` in metres-per-metre once `inv_x` / `inv_y` carry
/// the `1 / (8 * pitch)` factor.
#[inline]
pub(super) fn horn_gradient(
    field: &ScalarField,
    x: u32,
    y: u32,
    inv_x: f32,
    inv_y: f32,
) -> (f32, f32) {
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
