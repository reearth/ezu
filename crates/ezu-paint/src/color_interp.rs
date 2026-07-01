//! Colour-stop interpolation in a selectable colour space.
//!
//! ezu historically interpolated colour stops (in `color-ramp` and the
//! `gradient-*` ops) with a straight per-channel sRGB lerp. This module adds
//! **hue-aware** spaces — HSL, HSV, and the perceptual HCL / LAB pair — so a
//! ramp can be interpolated the way MapLibre (and most design tools) do.
//!
//! The HCL / LAB conversions and the HCL shortest-path hue wrap are ported
//! from the MapLibre style spec (`color_spaces.ts` + `Color.interpolate`),
//! so a converted MapLibre `interpolate-hcl` / `interpolate-lab` ramp lands
//! on the same colours. All colours here are **non-premultiplied**
//! `[r, g, b, alpha]` in `0..1`.

// The LAB/XYZ matrix coefficients are reproduced verbatim from the MapLibre
// style spec; keep their published precision even past what f32 resolves.
#![allow(clippy::excessive_precision)]

/// Colour space a stop table is interpolated in. `Rgb` is the historical
/// default (straight sRGB lerp); the others interpolate hue on the shortest
/// path around the wheel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InterpSpace {
    #[default]
    Rgb,
    Hsl,
    Hsv,
    Hcl,
    Lab,
}

impl InterpSpace {
    /// Parse a style field value (`"rgb"`, `"hsl"`, `"hsv"`, `"hcl"`,
    /// `"lab"`). Case-insensitive. Returns `None` for anything else.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "rgb" => Some(Self::Rgb),
            "hsl" => Some(Self::Hsl),
            "hsv" | "hsb" => Some(Self::Hsv),
            "hcl" | "lch" => Some(Self::Hcl),
            "lab" => Some(Self::Lab),
            _ => None,
        }
    }

    /// Stable byte tag for cache/param hashing.
    pub fn hash_tag(self) -> u8 {
        match self {
            Self::Rgb => 0,
            Self::Hsl => 1,
            Self::Hsv => 2,
            Self::Hcl => 3,
            Self::Lab => 4,
        }
    }
}

/// Interpolate two non-premultiplied RGBA colours at `t` in `space`.
/// Hue-based spaces take the shortest path around the wheel and treat an
/// achromatic endpoint (undefined hue) as sharing the other endpoint's hue.
pub fn interpolate(from: [f32; 4], to: [f32; 4], t: f32, space: InterpSpace) -> [f32; 4] {
    match space {
        InterpSpace::Rgb => lerp4(from, to, t),
        InterpSpace::Lab => {
            let a = rgb_to_lab(from);
            let b = rgb_to_lab(to);
            lab_to_rgb(lerp4(a, b, t))
        }
        InterpSpace::Hcl => interp_hcl(from, to, t),
        InterpSpace::Hsl => {
            let a = rgb_to_hsl(from);
            let b = rgb_to_hsl(to);
            hsl_to_rgb(interp_cylindrical(a, b, t))
        }
        InterpSpace::Hsv => {
            let a = rgb_to_hsv(from);
            let b = rgb_to_hsv(to);
            hsv_to_rgb(interp_cylindrical(a, b, t))
        }
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a * (1.0 - t) + b * t
}

fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        lerp(a[0], b[0], t),
        lerp(a[1], b[1], t),
        lerp(a[2], b[2], t),
        lerp(a[3], b[3], t),
    ]
}

/// Shortest-path hue interpolation (degrees). Either input may be `NaN`
/// (achromatic); the defined one is used, and `NaN`+`NaN` stays `NaN`.
fn interp_hue(h0: f32, h1: f32, t: f32) -> f32 {
    if h0.is_nan() {
        return h1;
    }
    if h1.is_nan() {
        return h0;
    }
    let mut dh = h1 - h0;
    if h1 > h0 && dh > 180.0 {
        dh -= 360.0;
    } else if h1 < h0 && h0 - h1 > 180.0 {
        dh += 360.0;
    }
    h0 + t * dh
}

/// Interpolate a `[hue°, c1, c2, alpha]` cylindrical colour (HSL/HSV).
fn interp_cylindrical(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        interp_hue(a[0], b[0], t),
        lerp(a[1], b[1], t),
        lerp(a[2], b[2], t),
        lerp(a[3], b[3], t),
    ]
}

// ---------------------------------------------------------------------------
// HSL / HSV. Hue in degrees [0, 360) or NaN when achromatic; S/L/V in [0, 1].

fn rgb_to_hsl([r, g, b, alpha]: [f32; 4]) -> [f32; 4] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    let d = max - min;
    if d < 1e-6 {
        return [f32::NAN, 0.0, l, alpha];
    }
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    [hue_from_rgb(r, g, b, max, d), s, l, alpha]
}

fn hsl_to_rgb([h, s, l, alpha]: [f32; 4]) -> [f32; 4] {
    let h = if h.is_nan() { 0.0 } else { h };
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let (r, g, b) = hue_chroma_to_rgb(h, c, l - c * 0.5);
    [r, g, b, alpha]
}

fn rgb_to_hsv([r, g, b, alpha]: [f32; 4]) -> [f32; 4] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let s = if max < 1e-6 { 0.0 } else { d / max };
    if d < 1e-6 {
        return [f32::NAN, s, max, alpha];
    }
    [hue_from_rgb(r, g, b, max, d), s, max, alpha]
}

fn hsv_to_rgb([h, s, v, alpha]: [f32; 4]) -> [f32; 4] {
    let h = if h.is_nan() { 0.0 } else { h };
    let c = v * s;
    let (r, g, b) = hue_chroma_to_rgb(h, c, v - c);
    [r, g, b, alpha]
}

/// Hue (degrees) from RGB given the channel max and delta. `d > 0`.
fn hue_from_rgb(r: f32, g: f32, b: f32, max: f32, d: f32) -> f32 {
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } * 60.0;
    h.rem_euclid(360.0)
}

/// Shared hue+chroma → RGB with a value/lightness offset `m`.
fn hue_chroma_to_rgb(h: f32, c: f32, m: f32) -> (f32, f32, f32) {
    let hh = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hh.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hh.floor() as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        (r1 + m).clamp(0.0, 1.0),
        (g1 + m).clamp(0.0, 1.0),
        (b1 + m).clamp(0.0, 1.0),
    )
}

// ---------------------------------------------------------------------------
// LAB / HCL, ported from maplibre-style-spec (D50 white point).
// See https://observablehq.com/@mbostock/lab-and-rgb

const XN: f32 = 0.96422;
const YN: f32 = 1.0;
const ZN: f32 = 0.82521;
const T0: f32 = 4.0 / 29.0;
const T1: f32 = 6.0 / 29.0;
const T2: f32 = 3.0 * T1 * T1;
const T3: f32 = T1 * T1 * T1;
const DEG2RAD: f32 = std::f32::consts::PI / 180.0;
const RAD2DEG: f32 = 180.0 / std::f32::consts::PI;

fn rgb2xyz(x: f32) -> f32 {
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

fn xyz2lab(t: f32) -> f32 {
    if t > T3 {
        t.cbrt()
    } else {
        t / T2 + T0
    }
}

fn lab2xyz(t: f32) -> f32 {
    if t > T1 {
        t * t * t
    } else {
        T2 * (t - T0)
    }
}

fn xyz2rgb(x: f32) -> f32 {
    let x = if x <= 0.00304 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    };
    x.clamp(0.0, 1.0)
}

fn rgb_to_lab([r, g, b, alpha]: [f32; 4]) -> [f32; 4] {
    let r = rgb2xyz(r);
    let g = rgb2xyz(g);
    let bb = rgb2xyz(b);
    let y = xyz2lab((0.2225045 * r + 0.7168786 * g + 0.0606169 * bb) / YN);
    let (x, z) = if r == g && g == bb {
        (y, y)
    } else {
        (
            xyz2lab((0.4360747 * r + 0.3850649 * g + 0.1430804 * bb) / XN),
            xyz2lab((0.0139322 * r + 0.0971045 * g + 0.7141733 * bb) / ZN),
        )
    };
    let l = 116.0 * y - 16.0;
    [
        if l < 0.0 { 0.0 } else { l },
        500.0 * (x - y),
        200.0 * (y - z),
        alpha,
    ]
}

fn lab_to_rgb([l, a, b, alpha]: [f32; 4]) -> [f32; 4] {
    let mut y = (l + 16.0) / 116.0;
    let mut x = if a.is_nan() { y } else { y + a / 500.0 };
    let mut z = if b.is_nan() { y } else { y - b / 200.0 };
    y = YN * lab2xyz(y);
    x = XN * lab2xyz(x);
    z = ZN * lab2xyz(z);
    [
        xyz2rgb(3.1338561 * x - 1.6168667 * y - 0.4906146 * z), // D50 -> sRGB
        xyz2rgb(-0.9787684 * x + 1.9161415 * y + 0.033454 * z),
        xyz2rgb(0.0719453 * x - 0.2289914 * y + 1.4052427 * z),
        alpha,
    ]
}

fn rgb_to_hcl(rgb: [f32; 4]) -> [f32; 4] {
    let [l, a, b, alpha] = rgb_to_lab(rgb);
    let c = (a * a + b * b).sqrt();
    let h = if (c * 10000.0).round() != 0.0 {
        (b.atan2(a) * RAD2DEG).rem_euclid(360.0)
    } else {
        f32::NAN
    };
    [h, c, l, alpha]
}

fn hcl_to_rgb([h, c, l, alpha]: [f32; 4]) -> [f32; 4] {
    let h = if h.is_nan() { 0.0 } else { h * DEG2RAD };
    lab_to_rgb([l, h.cos() * c, h.sin() * c, alpha])
}

/// HCL interpolation, faithful to `Color.interpolate('hcl')` including the
/// pure-black/white chroma preservation.
fn interp_hcl(from: [f32; 4], to: [f32; 4], t: f32) -> [f32; 4] {
    let [h0, c0, l0, a0] = rgb_to_hcl(from);
    let [h1, c1, l1, a1] = rgb_to_hcl(to);
    let mut chroma_override: Option<f32> = None;
    let hue = if !h0.is_nan() && !h1.is_nan() {
        interp_hue(h0, h1, t)
    } else if !h0.is_nan() {
        if l1 == 1.0 || l1 == 0.0 {
            chroma_override = Some(c0);
        }
        h0
    } else if !h1.is_nan() {
        if l0 == 1.0 || l0 == 0.0 {
            chroma_override = Some(c1);
        }
        h1
    } else {
        f32::NAN
    };
    let chroma = chroma_override.unwrap_or_else(|| lerp(c0, c1, t));
    hcl_to_rgb([hue, chroma, lerp(l0, l1, t), lerp(a0, a1, t)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 4], b: [f32; 4], eps: f32) -> bool {
        (0..4).all(|i| (a[i] - b[i]).abs() <= eps)
    }

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

    #[test]
    fn endpoints_exact_in_every_space() {
        for space in [
            InterpSpace::Rgb,
            InterpSpace::Hsl,
            InterpSpace::Hsv,
            InterpSpace::Hcl,
            InterpSpace::Lab,
        ] {
            assert!(
                close(interpolate(RED, BLUE, 0.0, space), RED, 3e-3),
                "{space:?} @0"
            );
            assert!(
                close(interpolate(RED, BLUE, 1.0, space), BLUE, 3e-3),
                "{space:?} @1"
            );
        }
    }

    #[test]
    fn rgb_is_plain_channel_mean() {
        let m = interpolate(
            [0.2, 0.4, 0.6, 1.0],
            [0.8, 0.6, 0.0, 0.4],
            0.5,
            InterpSpace::Rgb,
        );
        assert!(close(m, [0.5, 0.5, 0.3, 0.7], 1e-6));
    }

    #[test]
    fn roundtrips() {
        for c in [
            [0.8, 0.2, 0.1, 1.0],
            [0.1, 0.5, 0.9, 1.0],
            [0.3, 0.7, 0.2, 0.5],
        ] {
            assert!(close(hsl_to_rgb(rgb_to_hsl(c)), c, 2e-3), "hsl {c:?}");
            assert!(close(hsv_to_rgb(rgb_to_hsv(c)), c, 2e-3), "hsv {c:?}");
            assert!(close(lab_to_rgb(rgb_to_lab(c)), c, 2e-3), "lab {c:?}");
            assert!(close(hcl_to_rgb(rgb_to_hcl(c)), c, 2e-3), "hcl {c:?}");
        }
    }

    #[test]
    fn hue_wraps_the_short_way() {
        // Red (hue 0) -> magenta-ish (hue ~320). Shortest path dips below 0
        // (wraps through 360), so the HSL midpoint hue should be near 340,
        // not ~160 (the long way through green/cyan).
        let magenta = hsv_to_rgb([320.0, 1.0, 1.0, 1.0]);
        let mid = interpolate(RED, magenta, 0.5, InterpSpace::Hsl);
        let mid_h = rgb_to_hsl(mid)[0];
        assert!(
            !(90.0..=270.0).contains(&mid_h),
            "hue {mid_h} took the long way"
        );
    }

    #[test]
    fn achromatic_endpoint_keeps_other_hue() {
        // Grey -> red in HCL: grey has undefined hue, so the ramp should
        // stay on red's hue (no random sweep) and just gain chroma.
        let grey = [0.5, 0.5, 0.5, 1.0];
        let mid = interpolate(grey, RED, 0.5, InterpSpace::Hcl);
        let h = rgb_to_hcl(mid)[0];
        // Red's hue in HCL is ~40; accept a wide band, just not NaN/opposite.
        assert!(!h.is_nan());
        assert!(!(90.0..=350.0).contains(&h), "unexpected hue {h}");
    }
}
