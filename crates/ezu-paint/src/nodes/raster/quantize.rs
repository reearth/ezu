//! `quantize` — `Raster|Sprite -> Raster|Sprite`. Snap every pixel to the
//! nearest colour in a fixed `palette`, measuring distance perceptually in
//! **CIELAB** (ΔE, the default) or in plain RGB. Source coverage (alpha) is
//! preserved. Great for limited-palette / poster / pixel-art looks — where
//! `posterize` (independent per-channel quantization) can't snap to a
//! chosen set of colours.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::color_interp::rgb_to_lab;
use crate::nodes::common::{
    raster_or_sprite_output, read_string_or, unwrap_raster_or_sprite, wrap_raster_like,
    ACCEPTS_RASTER_OR_SPRITE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Metric {
    Rgb,
    Lab,
}

struct QuantizeNode {
    /// Straight (non-premultiplied) RGB of each palette entry, 0..1.
    palette: Vec<[f32; 3]>,
    /// Same entries projected into the distance metric's space.
    coords: Vec<[f32; 3]>,
    metric: Metric,
}

impl QuantizeNode {
    fn nearest(&self, rgb: [f32; 3]) -> [f32; 3] {
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
        self.palette[best]
    }
}

impl Node for QuantizeNode {
    fn op_name(&self) -> &'static str {
        "quantize"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "input",
            accepts: ACCEPTS_RASTER_OR_SPRITE,
            optional: false,
        }];
        SPECS
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        raster_or_sprite_output(input_kinds)
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let (src, kind) = unwrap_raster_or_sprite(input, "input")?;
        let mut out = RasterBuf::new(src.width, src.height);
        for i in (0..src.pixels.len()).step_by(4) {
            let a = src.pixels[i + 3] as f32 / 255.0;
            if a <= 0.0 {
                continue; // stays transparent
            }
            let rgb = [
                (src.pixels[i] as f32 / 255.0 / a).min(1.0),
                (src.pixels[i + 1] as f32 / 255.0 / a).min(1.0),
                (src.pixels[i + 2] as f32 / 255.0 / a).min(1.0),
            ];
            let q = self.nearest(rgb);
            // Re-premultiply with the source alpha (preserve coverage).
            out.pixels[i] = (q[0] * a * 255.0).round() as u8;
            out.pixels[i + 1] = (q[1] * a * 255.0).round() as u8;
            out.pixels[i + 2] = (q[2] * a * 255.0).round() as u8;
            out.pixels[i + 3] = src.pixels[i + 3];
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"quantize");
        h.update(&[match self.metric {
            Metric::Rgb => 0,
            Metric::Lab => 1,
        }]);
        for c in &self.palette {
            for v in c {
                h.update(&v.to_le_bytes());
            }
        }
    }
}

/// Project a straight RGB (0..1) into the distance metric's space.
fn project(rgb: [f32; 3], metric: Metric) -> [f32; 3] {
    match metric {
        Metric::Rgb => rgb,
        Metric::Lab => {
            let lab = rgb_to_lab([rgb[0], rgb[1], rgb[2], 1.0]);
            [lab[0], lab[1], lab[2]]
        }
    }
}

pub(super) struct QuantizeFactory;
impl NodeFactory for QuantizeFactory {
    fn op_name(&self) -> &'static str {
        "quantize"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
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
        let mut palette = Vec::with_capacity(arr.len());
        for (i, v) in arr.iter().enumerate() {
            let s = v.as_str().ok_or_else(|| FactoryError::BadField {
                field: format!("palette[{i}]"),
                msg: "expected `#rrggbb[aa]` string".into(),
            })?;
            palette.push(parse_hex_rgb(s).ok_or_else(|| FactoryError::BadField {
                field: format!("palette[{i}]"),
                msg: format!("bad colour: {s}"),
            })?);
        }
        let metric_str = read_string_or(fields, "space", ctx, "lab")?;
        let metric = match metric_str.as_str() {
            "lab" => Metric::Lab,
            "rgb" => Metric::Rgb,
            other => {
                return Err(FactoryError::BadField {
                    field: "space".into(),
                    msg: format!("distance space must be `lab` or `rgb`, got `{other}`"),
                })
            }
        };
        let coords = palette.iter().map(|&c| project(c, metric)).collect();
        Ok(BuiltNode {
            node: Box::new(QuantizeNode {
                palette,
                coords,
                metric,
            }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Snap each pixel to the nearest colour in `palette`, measuring distance in `space` (`lab` = perceptual ΔE, default; or `rgb`). Alpha is preserved. Use for limited-palette / poster / pixel-art looks.",
            "properties": {
                "input": schema_frag::node_ref(),
                "palette": {
                    "type": "array",
                    "minItems": 1,
                    "items": { "type": "string", "description": "`#rrggbb` or `#rrggbbaa` (alpha ignored for matching)." },
                },
                "space": { "type": "string", "enum": ["lab", "rgb"], "default": "lab", "description": "Colour space the nearest-colour distance is measured in." },
            },
            "required": ["input", "palette"],
        })
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

ezu_graph::submit_node!(QuantizeFactory);
