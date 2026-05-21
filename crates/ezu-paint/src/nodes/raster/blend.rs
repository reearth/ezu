//! `blend` — composite `over` onto `base` with a Photoshop-style blend
//! mode, optional clipping (source-atop), and optional alpha mask.
//!
//! Blend math follows the W3C *Compositing and Blending Level 1*
//! reference. Inputs are premultiplied sRGB8; the implementation
//! demultiplies, applies the blend in non-premultiplied space, then
//! recomposites with source-over (or source-atop when `clip` is set).
//!
//! Modes implemented (16, full W3C set):
//! - Separable: `normal`, `multiply`, `screen`, `overlay`, `darken`,
//!   `lighten`, `color-dodge`, `color-burn`, `hard-light`, `soft-light`,
//!   `difference`, `exclusion`
//! - Non-separable: `hue`, `saturation`, `color`, `luminosity`

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, take_optional_input_ref, BuiltNode, Connection, EvalCtx,
    EvalError, FactoryCtx, FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
    RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{read_number_or, read_string_or};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "normal" => Self::Normal,
            "multiply" => Self::Multiply,
            "screen" => Self::Screen,
            "overlay" => Self::Overlay,
            "darken" => Self::Darken,
            "lighten" => Self::Lighten,
            "color-dodge" => Self::ColorDodge,
            "color-burn" => Self::ColorBurn,
            "hard-light" => Self::HardLight,
            "soft-light" => Self::SoftLight,
            "difference" => Self::Difference,
            "exclusion" => Self::Exclusion,
            "hue" => Self::Hue,
            "saturation" => Self::Saturation,
            "color" => Self::Color,
            "luminosity" => Self::Luminosity,
            _ => return None,
        })
    }

    fn as_tag(self) -> &'static [u8] {
        match self {
            Self::Normal => b"normal",
            Self::Multiply => b"multiply",
            Self::Screen => b"screen",
            Self::Overlay => b"overlay",
            Self::Darken => b"darken",
            Self::Lighten => b"lighten",
            Self::ColorDodge => b"color-dodge",
            Self::ColorBurn => b"color-burn",
            Self::HardLight => b"hard-light",
            Self::SoftLight => b"soft-light",
            Self::Difference => b"difference",
            Self::Exclusion => b"exclusion",
            Self::Hue => b"hue",
            Self::Saturation => b"saturation",
            Self::Color => b"color",
            Self::Luminosity => b"luminosity",
        }
    }
}

struct BlendNode {
    mode: BlendMode,
    opacity: f32,
    clip: bool,
    has_mask: bool,
}

impl Node for BlendNode {
    fn op_name(&self) -> &'static str {
        "blend"
    }
    fn inputs(&self) -> &[PortSpec] {
        // PortSpec layout is fixed across instances; the optional `mask`
        // is declared optional at the port level regardless of whether
        // the style connects it.
        static SPECS: &[PortSpec] = &[
            PortSpec {
                name: "base",
                kind: PortKind::Raster,
                optional: false,
            },
            PortSpec {
                name: "over",
                kind: PortKind::Raster,
                optional: false,
            },
            PortSpec {
                name: "mask",
                kind: PortKind::Raster,
                optional: true,
            },
        ];
        SPECS
    }
    fn output(&self) -> PortKind {
        PortKind::Raster
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let base = inputs[0]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("base".into()))?;
        let over = inputs[1]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("over".into()))?;
        let mask = if self.has_mask {
            Some(
                inputs[2]
                    .as_ref()
                    .and_then(PortValue::as_raster)
                    .ok_or_else(|| EvalError::MissingInput("mask".into()))?,
            )
        } else {
            None
        };
        if base.width != over.width || base.height != over.height {
            return Err(EvalError::Other("blend: base/over size mismatch".into()));
        }
        if let Some(m) = mask {
            if m.width != base.width || m.height != base.height {
                return Err(EvalError::Other("blend: mask size mismatch".into()));
            }
        }
        let mut out = RasterBuf::new(base.width, base.height);
        let op = self.opacity.clamp(0.0, 1.0);
        for i in (0..base.pixels.len()).step_by(4) {
            // Demultiply base + over to [0,1] RGB + alpha.
            let (br, bg, bb, ba) = demul(&base.pixels[i..i + 4]);
            let (sr, sg, sb, sa_raw) = demul(&over.pixels[i..i + 4]);
            // Source effective alpha = sa * opacity * mask.alpha (mask
            // contributes coverage, not color).
            let mask_a = match mask {
                Some(m) => m.pixels[i + 3] as f32 / 255.0,
                None => 1.0,
            };
            let sa = sa_raw * op * mask_a;
            // Apply blend function to non-premultiplied colors.
            let (mr, mg, mb) = blend_color(self.mode, [br, bg, bb], [sr, sg, sb]);
            // Blended source per W3C: Cs' = (1 - αb) * Cs + αb * B(Cb, Cs).
            let bsr = (1.0 - ba) * sr + ba * mr;
            let bsg = (1.0 - ba) * sg + ba * mg;
            let bsb = (1.0 - ba) * sb + ba * mb;
            // Composite. `clip` switches source-over -> source-atop.
            let (or, og, ob, oa) = if self.clip {
                // source-atop: αo = αb, co = αs*αb*Cs' + (1-αs)*αb*Cb
                let oa = ba;
                let or = sa * ba * bsr + (1.0 - sa) * ba * br;
                let og = sa * ba * bsg + (1.0 - sa) * ba * bg;
                let ob = sa * ba * bsb + (1.0 - sa) * ba * bb;
                (or, og, ob, oa)
            } else {
                // source-over: αo = αs + αb*(1-αs)
                let oa = sa + ba * (1.0 - sa);
                let or = sa * bsr + (1.0 - sa) * ba * br;
                let og = sa * bsg + (1.0 - sa) * ba * bg;
                let ob = sa * bsb + (1.0 - sa) * ba * bb;
                (or, og, ob, oa)
            };
            // `or`/`og`/`ob` are already premultiplied (multiplied by
            // alphas in the composite step). Pack back into u8.
            out.pixels[i] = to_u8(or);
            out.pixels[i + 1] = to_u8(og);
            out.pixels[i + 2] = to_u8(ob);
            out.pixels[i + 3] = to_u8(oa);
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"blend");
        h.update(self.mode.as_tag());
        h.update(&self.opacity.to_le_bytes());
        h.update(&[self.clip as u8, self.has_mask as u8]);
    }
}

pub(super) struct BlendFactory;
impl NodeFactory for BlendFactory {
    fn op_name(&self) -> &'static str {
        "blend"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let base = take_input_ref(fields, "base")?;
        let over = take_input_ref(fields, "over")?;
        let mask = take_optional_input_ref(fields, "mask")?;
        let opacity = read_number_or(fields, "opacity", ctx, 1.0)? as f32;
        let mode_str = read_string_or(fields, "mode", ctx, "normal")?;
        let mode = BlendMode::parse(&mode_str).ok_or_else(|| FactoryError::BadField {
            field: "mode".into(),
            msg: format!("unknown blend mode `{mode_str}`"),
        })?;
        let clip = fields
            .get("clip")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let has_mask = mask.is_some();
        let mut connections = vec![
            Connection {
                port: "base".into(),
                src: base,
            },
            Connection {
                port: "over".into(),
                src: over,
            },
        ];
        if let Some(m) = mask {
            connections.push(Connection {
                port: "mask".into(),
                src: m,
            });
        }
        Ok(BuiltNode {
            node: Box::new(BlendNode {
                mode,
                opacity,
                clip,
                has_mask,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Composite `over` onto `base` with a W3C blend mode. `clip: true` clips result to base alpha (Photoshop-style clipping mask). Optional `mask` raster's alpha modulates source coverage.",
            "properties": {
                "base": schema_frag::node_ref(),
                "over": schema_frag::node_ref(),
                "mask": schema_frag::node_ref(),
                "mode": {
                    "type": "string",
                    "enum": [
                        "normal","multiply","screen","overlay","darken","lighten",
                        "color-dodge","color-burn","hard-light","soft-light",
                        "difference","exclusion",
                        "hue","saturation","color","luminosity"
                    ],
                    "default": "normal"
                },
                "clip": { "type": "boolean", "default": false },
                "opacity": schema_frag::unit_number(),
            },
            "required": ["base", "over"],
        })
    }
}

// ---------------------------------------------------------------------------
// Pixel helpers.

#[inline]
fn demul(px: &[u8]) -> (f32, f32, f32, f32) {
    let a = px[3] as f32 / 255.0;
    if a <= 0.0 {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let r = (px[0] as f32 / 255.0) / a;
    let g = (px[1] as f32 / 255.0) / a;
    let b = (px[2] as f32 / 255.0) / a;
    (r.min(1.0), g.min(1.0), b.min(1.0), a)
}

#[inline]
fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

// ---------------------------------------------------------------------------
// Blend math — W3C Compositing and Blending Level 1.

fn blend_color(mode: BlendMode, b: [f32; 3], s: [f32; 3]) -> (f32, f32, f32) {
    match mode {
        BlendMode::Hue => set_lum(set_sat(s, sat(b)), lum(b)),
        BlendMode::Saturation => set_lum(set_sat(b, sat(s)), lum(b)),
        BlendMode::Color => set_lum(s, lum(b)),
        BlendMode::Luminosity => set_lum(b, lum(s)),
        sep => (
            blend_separable(sep, b[0], s[0]),
            blend_separable(sep, b[1], s[1]),
            blend_separable(sep, b[2], s[2]),
        ),
    }
}

fn blend_separable(mode: BlendMode, b: f32, s: f32) -> f32 {
    match mode {
        BlendMode::Normal => s,
        BlendMode::Multiply => b * s,
        BlendMode::Screen => b + s - b * s,
        BlendMode::Overlay => blend_separable(BlendMode::HardLight, s, b),
        BlendMode::Darken => b.min(s),
        BlendMode::Lighten => b.max(s),
        BlendMode::ColorDodge => {
            if b <= 0.0 {
                0.0
            } else if s >= 1.0 {
                1.0
            } else {
                (b / (1.0 - s)).min(1.0)
            }
        }
        BlendMode::ColorBurn => {
            if b >= 1.0 {
                1.0
            } else if s <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - b) / s).min(1.0)
            }
        }
        BlendMode::HardLight => {
            if s <= 0.5 {
                2.0 * b * s
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - s)
            }
        }
        BlendMode::SoftLight => {
            // W3C formula. d(b) branch for the 2nd half.
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 {
                    ((16.0 * b - 12.0) * b + 4.0) * b
                } else {
                    b.sqrt()
                };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }
        BlendMode::Difference => (b - s).abs(),
        BlendMode::Exclusion => b + s - 2.0 * b * s,
        // Non-separable handled by blend_color.
        BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity => s,
    }
}

#[inline]
fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn set_lum(c: [f32; 3], l: f32) -> (f32, f32, f32) {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

fn clip_color(c: [f32; 3]) -> (f32, f32, f32) {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut r = c[0];
    let mut g = c[1];
    let mut b = c[2];
    if n < 0.0 {
        r = l + (r - l) * l / (l - n);
        g = l + (g - l) * l / (l - n);
        b = l + (b - l) * l / (l - n);
    }
    if x > 1.0 {
        r = l + (r - l) * (1.0 - l) / (x - l);
        g = l + (g - l) * (1.0 - l) / (x - l);
        b = l + (b - l) * (1.0 - l) / (x - l);
    }
    (r, g, b)
}

#[inline]
fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    // Sort channels by value, keep indices, rescale [min, mid, max].
    let mut idx = [0, 1, 2];
    idx.sort_by(|&i, &j| c[i].partial_cmp(&c[j]).unwrap_or(std::cmp::Ordering::Equal));
    let (lo, mid, hi) = (idx[0], idx[1], idx[2]);
    let mut out = [0.0f32; 3];
    if c[hi] > c[lo] {
        out[mid] = (c[mid] - c[lo]) * s / (c[hi] - c[lo]);
        out[hi] = s;
    }
    out[lo] = 0.0;
    out
}

ezu_graph::submit_node!(BlendFactory);
