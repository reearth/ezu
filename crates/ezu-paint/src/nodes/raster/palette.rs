//! Shared fixed-palette nearest-colour matching for `quantize` and
//! `dither`. Distance is measured in perceptual CIELAB (ΔE, the default) or
//! plain RGB.

use ezu_graph::{EnumTable, EvalCtx, EvalError, FactoryError, In, InEnum, InReader, PortValue};
use serde_json::{Map, Value};
use xxhash_rust::xxh3::Xxh3;

use crate::color_interp::rgb_to_lab;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Metric {
    Rgb,
    Lab,
}

/// The distance spaces a palette match can measure in.
pub(super) const METRICS: EnumTable<Metric> = &[("lab", Metric::Lab), ("rgb", Metric::Rgb)];

/// A declared colour palette. Each entry is a literal or a `$param`, so
/// the table — and its projection into the metric space — is resolved
/// once per eval by [`Palette::resolve`].
pub(super) struct Palette {
    colors: Vec<In<[f32; 4]>>,
    metric: InEnum<Metric>,
}

/// A palette resolved for one eval, with each entry projected into the
/// distance metric's space for fast nearest-colour lookup.
pub(super) struct ResolvedPalette {
    /// Straight (non-premultiplied) RGB of each entry, 0..1.
    colors: Vec<[f32; 3]>,
    /// Entries in the metric space (RGB or LAB).
    coords: Vec<[f32; 3]>,
    metric: Metric,
}

impl Palette {
    /// Read `palette` (array of `#rrggbb[aa]` or `$param`) and the
    /// `space` distance metric (`lab` default, or `rgb`) from a node's
    /// fields.
    pub(super) fn from_fields(
        fields: &Map<String, Value>,
        r: &mut InReader<'_, '_>,
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
            colors.push(r.nested(&format!("palette[{i}]"), v)?);
        }
        let metric = r.enum_or("space", Metric::Lab, METRICS)?;
        Ok(Self { colors, metric })
    }

    /// Resolve every entry for one eval and project it into the metric
    /// space. Call once per eval, never per pixel.
    pub(super) fn resolve(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<ResolvedPalette, EvalError> {
        let mut colors = Vec::with_capacity(self.colors.len());
        for c in &self.colors {
            let rgba = c.get(ctx, inputs)?;
            colors.push([rgba[0], rgba[1], rgba[2]]);
        }
        let metric = self.metric.get(ctx)?;
        let coords = colors.iter().map(|&c| project(c, metric)).collect();
        Ok(ResolvedPalette {
            colors,
            coords,
            metric,
        })
    }

    pub(super) fn hash(&self, h: &mut Xxh3) {
        self.metric.param_hash(h);
        for c in &self.colors {
            c.param_hash(h);
        }
    }
}

impl ResolvedPalette {
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
