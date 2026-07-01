//! Shared fixed-palette nearest-colour matching for `quantize` and
//! `dither`. Distance is measured in perceptual CIELAB (ΔE, the default) or
//! plain RGB.

use ezu_graph::{FactoryCtx, FactoryError};
use serde_json::{Map, Value};
use xxhash_rust::xxh3::Xxh3;

use crate::color_interp::rgb_to_lab;
use crate::nodes::common::read_string_or;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Metric {
    Rgb,
    Lab,
}

/// A parsed colour palette with each entry projected into the distance
/// metric's space for fast nearest-colour lookup.
pub(super) struct Palette {
    /// Straight (non-premultiplied) RGB of each entry, 0..1.
    colors: Vec<[f32; 3]>,
    /// Entries in the metric space (RGB or LAB).
    coords: Vec<[f32; 3]>,
    metric: Metric,
}

impl Palette {
    /// Read `palette` (array of `#rrggbb[aa]`) and the `space` distance
    /// metric (`lab` default, or `rgb`) from a node's fields.
    pub(super) fn from_fields(
        fields: &Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<Self, FactoryError> {
        let raw = fields
            .get("palette")
            .ok_or_else(|| FactoryError::MissingField("palette".into()))?;
        let arr = raw.as_array().ok_or_else(|| FactoryError::BadField {
            field: "palette".into(),
            msg: "expected an array of `#rrggbb` colour strings".into(),
        })?;
        if arr.is_empty() {
            return Err(FactoryError::BadField {
                field: "palette".into(),
                msg: "at least one colour required".into(),
            });
        }
        let mut colors = Vec::with_capacity(arr.len());
        for (i, v) in arr.iter().enumerate() {
            let s = v.as_str().ok_or_else(|| FactoryError::BadField {
                field: format!("palette[{i}]"),
                msg: "expected `#rrggbb[aa]` string".into(),
            })?;
            colors.push(parse_hex_rgb(s).ok_or_else(|| FactoryError::BadField {
                field: format!("palette[{i}]"),
                msg: format!("bad colour: {s}"),
            })?);
        }
        let metric = match read_string_or(fields, "space", ctx, "lab")?.as_str() {
            "lab" => Metric::Lab,
            "rgb" => Metric::Rgb,
            other => {
                return Err(FactoryError::BadField {
                    field: "space".into(),
                    msg: format!("distance space must be `lab` or `rgb`, got `{other}`"),
                })
            }
        };
        let coords = colors.iter().map(|&c| project(c, metric)).collect();
        Ok(Self {
            colors,
            coords,
            metric,
        })
    }

    /// Nearest palette colour to a straight RGB pixel (0..1).
    pub(super) fn nearest(&self, rgb: [f32; 3]) -> [f32; 3] {
        let p = project(rgb, self.metric);
        let mut best = 0usize;
        let mut best_d = f32::INFINITY;
        for (i, c) in self.coords.iter().enumerate() {
            let d = (p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2) + (p[2] - c[2]).powi(2);
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        self.colors[best]
    }

    pub(super) fn hash(&self, h: &mut Xxh3) {
        h.update(&[match self.metric {
            Metric::Rgb => 0,
            Metric::Lab => 1,
        }]);
        for c in &self.colors {
            for v in c {
                h.update(&v.to_le_bytes());
            }
        }
    }
}

fn project(rgb: [f32; 3], metric: Metric) -> [f32; 3] {
    match metric {
        Metric::Rgb => rgb,
        Metric::Lab => {
            let lab = rgb_to_lab([rgb[0], rgb[1], rgb[2], 1.0]);
            [lab[0], lab[1], lab[2]]
        }
    }
}

fn parse_hex_rgb(s: &str) -> Option<[f32; 3]> {
    let s = s.strip_prefix('#')?;
    let hex = match s.len() {
        3 => s.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 | 8 => s[..6].to_string(),
        _ => return None,
    };
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0])
}
