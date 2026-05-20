//! Extract paint-friendly defaults from a `hokusai` [`Brush`].
//!
//! `.myb` parsing is hokusai's job (`hokusai::myb::from_str`) — this module
//! only converts a parsed brush's base settings into the shape ezu's fill
//! code wants (linear-sRGB color, pixel radius, opacity, hardness).

use hokusai::{Brush, BrushSetting};

use crate::DabFillStyle;

/// Snapshot of a brush's appearance-controlling base values in convenient units:
///
/// - `radius_px`: brush radius in pixels (after exp-decoding `radius_logarithmic`)
/// - `opacity`:   base opacity in `[0, 1]`
/// - `hardness`:  base hardness in `[0, 1]`
/// - `hsv`:       `(h, s, v)` each in `[0, 1]` (libmypaint HSV convention)
#[derive(Debug, Clone, Copy)]
pub struct BrushDefaults {
    pub radius_px: f32,
    pub opacity: f32,
    pub hardness: f32,
    pub hsv: (f32, f32, f32),
}

impl BrushDefaults {
    pub fn from_brush(brush: &Brush) -> Self {
        let radius_log = brush.get(BrushSetting::Radius).base_value;
        let radius_px = radius_log.exp().clamp(0.2, 1000.0);
        let opacity = brush.get(BrushSetting::Opaque).base_value.clamp(0.0, 1.0);
        let hardness = brush.get(BrushSetting::Hardness).base_value.clamp(0.0, 1.0);
        let h = brush.get(BrushSetting::ColorH).base_value.rem_euclid(1.0);
        let s = brush.get(BrushSetting::ColorS).base_value.clamp(0.0, 1.0);
        let v = brush.get(BrushSetting::ColorV).base_value.clamp(0.0, 1.0);
        Self {
            radius_px,
            opacity,
            hardness,
            hsv: (h, s, v),
        }
    }

    /// Convert the brush's base HSV color into linear-sRGB RGB in `[0, 1]`.
    pub fn linear_rgb(&self) -> [f32; 3] {
        let (h, s, v) = self.hsv;
        let (r, g, b) = hsv_to_srgb(h, s, v);
        [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b)]
    }
}

impl DabFillStyle {
    /// Build a [`DabFillStyle`] using a brush's base values, then override the
    /// color with the caller-supplied linear-sRGB triple. Spacing / jitter
    /// stay at sensible scatter-fill defaults.
    pub fn from_brush_with_color(brush: &Brush, color_linear_rgb: [f32; 3]) -> Self {
        let d = BrushDefaults::from_brush(brush);
        let mut s = DabFillStyle::default();
        s.radius_px = d.radius_px.max(2.0);
        s.opacity = d.opacity.max(0.05);
        s.hardness = d.hardness.max(0.05);
        s.color = hokusai::color::RgbaF32::new(
            color_linear_rgb[0],
            color_linear_rgb[1],
            color_linear_rgb[2],
            1.0,
        );
        // Spacing scales with radius so scatter density adapts to brush size.
        s.spacing_px = (s.radius_px * 0.6).max(1.5);
        s
    }
}

fn hsv_to_srgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let h6 = (h.rem_euclid(1.0)) * 6.0;
    let c = v * s;
    let x = c * (1.0 - (h6 % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match h6 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    (r1 + m, g1 + m, b1 + m)
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}
