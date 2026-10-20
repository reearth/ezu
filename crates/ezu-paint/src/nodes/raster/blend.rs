//! `blend` — composite `over` onto `base` with a Photoshop-style blend
//! mode, a Porter-Duff operator, and optional alpha mask.
//!
//! Blend math follows the W3C *Compositing and Blending Level 1*
//! reference. Inputs are premultiplied sRGB8; the implementation
//! demultiplies, applies the blend in non-premultiplied space, then
//! recomposites with the chosen operator (source-over by default).
//!
//! Modes implemented (16, full W3C set):
//! - Separable: `normal`, `multiply`, `screen`, `overlay`, `darken`,
//!   `lighten`, `color-dodge`, `color-burn`, `hard-light`, `soft-light`,
//!   `difference`, `exclusion`
//! - Non-separable: `hue`, `saturation`, `color`, `luminosity`
//!
//! Operators implemented (12, the full Porter-Duff set): `clear`, `copy`,
//! `destination`, `source-over`, `destination-over`, `source-in`,
//! `destination-in`, `source-out`, `destination-out`, `source-atop`,
//! `destination-atop`, `xor`.

use std::sync::Arc;

use ezu_graph::{
    enum_names, schema_frag, take_input_ref, take_optional_input_ref, BuiltNode, Connection,
    EnumTable, EvalCtx, EvalError, FactoryCtx, FactoryError, In, InEnum, InReader, Node,
    NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    raster_or_sprite_output, unwrap_raster_or_sprite, wrap_raster_like, ACCEPTS_RASTER_OR_SPRITE,
};

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

const MODES: EnumTable<BlendMode> = &[
    ("normal", BlendMode::Normal),
    ("multiply", BlendMode::Multiply),
    ("screen", BlendMode::Screen),
    ("overlay", BlendMode::Overlay),
    ("darken", BlendMode::Darken),
    ("lighten", BlendMode::Lighten),
    ("color-dodge", BlendMode::ColorDodge),
    ("color-burn", BlendMode::ColorBurn),
    ("hard-light", BlendMode::HardLight),
    ("soft-light", BlendMode::SoftLight),
    ("difference", BlendMode::Difference),
    ("exclusion", BlendMode::Exclusion),
    ("hue", BlendMode::Hue),
    ("saturation", BlendMode::Saturation),
    ("color", BlendMode::Color),
    ("luminosity", BlendMode::Luminosity),
];

/// Porter-Duff compositing operator, by its W3C Compositing name.
/// Defaults to `SourceOver` (the usual "draw on top"). `DestinationOut`
/// is the eraser: keeps base where `over` is transparent, drops it where
/// `over` is opaque. `SourceAtop` is the clipping mask: `over` shows only
/// where base is, and the result keeps base's alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Composite {
    Clear,
    Copy,
    Destination,
    SourceOver,
    DestinationOver,
    SourceIn,
    DestinationIn,
    SourceOut,
    DestinationOut,
    SourceAtop,
    DestinationAtop,
    Xor,
}

/// The W3C names first, then the shorter spellings accepted for three of
/// them.
const COMPOSITES: EnumTable<Composite> = &[
    ("clear", Composite::Clear),
    ("copy", Composite::Copy),
    ("destination", Composite::Destination),
    ("source-over", Composite::SourceOver),
    ("destination-over", Composite::DestinationOver),
    ("source-in", Composite::SourceIn),
    ("destination-in", Composite::DestinationIn),
    ("source-out", Composite::SourceOut),
    ("destination-out", Composite::DestinationOut),
    ("source-atop", Composite::SourceAtop),
    ("destination-atop", Composite::DestinationAtop),
    ("xor", Composite::Xor),
    ("over", Composite::SourceOver),
    ("source", Composite::Copy),
    ("dest-out", Composite::DestinationOut),
    ("erase", Composite::DestinationOut),
];

impl Composite {
    /// The Porter-Duff coverage factors `(Fa, Fb)` for source alpha `sa`
    /// and backdrop alpha `ba`: the result is `Fa` of the (blended)
    /// source plus `Fb` of the backdrop.
    #[inline]
    fn factors(self, sa: f32, ba: f32) -> (f32, f32) {
        match self {
            Self::Clear => (0.0, 0.0),
            Self::Copy => (1.0, 0.0),
            Self::Destination => (0.0, 1.0),
            Self::SourceOver => (1.0, 1.0 - sa),
            Self::DestinationOver => (1.0 - ba, 1.0),
            Self::SourceIn => (ba, 0.0),
            Self::DestinationIn => (0.0, sa),
            Self::SourceOut => (1.0 - ba, 0.0),
            Self::DestinationOut => (0.0, 1.0 - sa),
            Self::SourceAtop => (ba, 1.0 - sa),
            Self::DestinationAtop => (1.0 - ba, sa),
            Self::Xor => (1.0 - ba, 1.0 - sa),
        }
    }
    /// Whether a fully transparent source leaves the backdrop as it is
    /// (`Fb` is 1 when the source alpha is 0). Under the others an empty
    /// source clears the backdrop.
    fn keeps_base_under_empty_source(self) -> bool {
        matches!(
            self,
            Self::Destination
                | Self::SourceOver
                | Self::DestinationOver
                | Self::DestinationOut
                | Self::SourceAtop
                | Self::Xor
        )
    }
}

struct BlendNode {
    mode: InEnum<BlendMode>,
    /// The operator the style names. `None` leaves it to the deprecated
    /// `clip` flag: source-atop when it is true, source-over otherwise.
    composite: Option<InEnum<Composite>>,
    opacity: In<f64>,
    clip: In<bool>,
    has_mask: bool,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for BlendNode {
    fn op_name(&self) -> &'static str {
        "blend"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        // Output mirrors `base`. Mixing a `Sprite` base with a
        // canvas-sized `Raster` over would normally fail the size
        // check at eval time anyway — the type system stays out of
        // that and just propagates `base`'s kind.
        raster_or_sprite_output(input_kinds)
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let base_in = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("base".into()))?;
        let (base, kind) = unwrap_raster_or_sprite(base_in, "base")?;
        let over_in = inputs[1]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("over".into()))?;
        let (over, _) = unwrap_raster_or_sprite(over_in, "over")?;
        let mask = if self.has_mask {
            let m_in = inputs[2]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("mask".into()))?;
            let (m, _) = unwrap_raster_or_sprite(m_in, "mask")?;
            Some(m)
        } else {
            None
        };
        let mask_ref = mask.as_deref();
        if base.width != over.width || base.height != over.height {
            return Err(EvalError::Other("blend: base/over size mismatch".into()));
        }
        if let Some(m) = mask_ref {
            if m.width != base.width || m.height != base.height {
                return Err(EvalError::Other("blend: mask size mismatch".into()));
            }
        }
        let op = (self.opacity.get(ctx, inputs)? as f32).clamp(0.0, 1.0);
        let mode = self.mode.get(ctx)?;
        let composite = match &self.composite {
            Some(c) => c.get(ctx)?,
            None if self.clip.get(ctx, inputs)? => Composite::SourceAtop,
            None => Composite::SourceOver,
        };

        // The backdrop alone, or nothing at all: no pixel of `over` counts.
        if composite == Composite::Destination {
            return Ok(base_in.clone());
        }
        if composite == Composite::Clear {
            let out = RasterBuf::new(base.width, base.height);
            return Ok(wrap_raster_like(Arc::new(out), kind));
        }

        // Fast path: `over` is fully transparent (all bytes zero), so
        // there is nothing to composite. Every mode reduces to "keep base"
        // when the source alpha is zero (mask and opacity only scale that
        // already-zero alpha) under the operators that keep the backdrop
        // there, so the result is `base` verbatim.
        if over.is_blank() && composite.keeps_base_under_empty_source() {
            return Ok(base_in.clone());
        }

        // Fast path: plain source-over of premultiplied bytes — the common
        // Normal / source-over / no-mask case. Skip the demultiply →
        // W3C blend → recomposite round-trip and work in integer space.
        if mode == BlendMode::Normal && composite == Composite::SourceOver && mask_ref.is_none() {
            // Base is transparent everywhere: source-over onto nothing is
            // just the (opacity-1) source. Reuse its buffer directly.
            if op >= 1.0 && base.is_blank() {
                return Ok(wrap_raster_like(over.clone(), kind));
            }
            let out = normal_over(&base, &over, op);
            return Ok(wrap_raster_like(Arc::new(out), kind));
        }

        let out = blend_general(&base, &over, mask_ref, mode, composite, op);
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"blend");
        self.mode.param_hash(h);
        match &self.composite {
            Some(c) => c.param_hash(h),
            None => h.update(b"clip-flag"),
        }
        self.opacity.param_hash(h);
        self.clip.param_hash(h);
        h.update(&[self.has_mask as u8]);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

/// General blend loop: demultiply, apply the W3C blend function in
/// non-premultiplied space, then recomposite with the Porter-Duff
/// operator `composite`.
fn blend_general(
    base: &RasterBuf,
    over: &RasterBuf,
    mask: Option<&RasterBuf>,
    mode: BlendMode,
    composite: Composite,
    op: f32,
) -> RasterBuf {
    let mut out = RasterBuf::new(base.width, base.height);
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
        // Short-circuit Porter-Duff destination-out (eraser): the
        // blend math is irrelevant — base is kept where over is
        // transparent, removed where over is opaque.
        if composite == Composite::DestinationOut {
            let inv = 1.0 - sa;
            out.pixels[i] = to_u8(br * ba * inv);
            out.pixels[i + 1] = to_u8(bg * ba * inv);
            out.pixels[i + 2] = to_u8(bb * ba * inv);
            out.pixels[i + 3] = to_u8(ba * inv);
            continue;
        }
        // Apply blend function to non-premultiplied colors.
        let (mr, mg, mb) = blend_color(mode, [br, bg, bb], [sr, sg, sb]);
        // Blended source per W3C: Cs' = (1 - αb) * Cs + αb * B(Cb, Cs).
        let bsr = (1.0 - ba) * sr + ba * mr;
        let bsg = (1.0 - ba) * sg + ba * mg;
        let bsb = (1.0 - ba) * sb + ba * mb;
        // Composite. Source-over and source-atop keep their own spelled-out
        // arithmetic so styles written before the other operators existed
        // round to the same bytes; the rest use the general Porter-Duff
        // form co = αs*Fa*Cs' + αb*Fb*Cb, αo = αs*Fa + αb*Fb.
        let (or, og, ob, oa) = match composite {
            Composite::SourceAtop => {
                // source-atop: αo = αb, co = αs*αb*Cs' + (1-αs)*αb*Cb
                let oa = ba;
                let or = sa * ba * bsr + (1.0 - sa) * ba * br;
                let og = sa * ba * bsg + (1.0 - sa) * ba * bg;
                let ob = sa * ba * bsb + (1.0 - sa) * ba * bb;
                (or, og, ob, oa)
            }
            Composite::SourceOver => {
                // source-over: αo = αs + αb*(1-αs)
                let oa = sa + ba * (1.0 - sa);
                let or = sa * bsr + (1.0 - sa) * ba * br;
                let og = sa * bsg + (1.0 - sa) * ba * bg;
                let ob = sa * bsb + (1.0 - sa) * ba * bb;
                (or, og, ob, oa)
            }
            other => {
                let (fa, fb) = other.factors(sa, ba);
                let s = sa * fa;
                let b = ba * fb;
                (s * bsr + b * br, s * bsg + b * bg, s * bsb + b * bb, s + b)
            }
        };
        // `or`/`og`/`ob` are already premultiplied (multiplied by
        // alphas in the composite step). Pack back into u8.
        out.pixels[i] = to_u8(or);
        out.pixels[i + 1] = to_u8(og);
        out.pixels[i + 2] = to_u8(ob);
        out.pixels[i + 3] = to_u8(oa);
    }
    out
}

/// Plain source-over of premultiplied RGBA8, done entirely in integer
/// space: `out_c = s_c + d_c * (255 - s_a) / 255`. When `op < 1`, the
/// premultiplied source is first scaled by `op` (scaling every channel,
/// alpha included, keeps it premultiplied). Matches [`blend_general`] for
/// `mode = Normal`, `composite = SourceOver` and no mask, to within a
/// rounding step per channel.
fn normal_over(base: &RasterBuf, over: &RasterBuf, op: f32) -> RasterBuf {
    let mut out = RasterBuf::new(base.width, base.height);
    // Fixed-point opacity in [0, 255]; `op >= 1` skips the scale entirely.
    let sf = if op >= 1.0 {
        255
    } else {
        (op * 255.0).round() as u32
    };
    for (o, (d, s)) in out.pixels.as_chunks_mut::<4>().0.iter_mut().zip(
        base.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(over.pixels.as_chunks::<4>().0),
    ) {
        let sa = if sf == 255 {
            s[3] as u32
        } else {
            div255(s[3] as u32 * sf)
        };
        let inv = 255 - sa;
        for c in 0..4 {
            let sc = if sf == 255 {
                s[c] as u32
            } else {
                div255(s[c] as u32 * sf)
            };
            o[c] = (sc + div255(d[c] as u32 * inv)) as u8;
        }
    }
    out
}

/// Rounded division by 255 for `x` in `[0, 65025]` (= 255 × 255).
#[inline]
pub(super) fn div255(x: u32) -> u32 {
    let t = x + 128;
    (t + (t >> 8)) >> 8
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
        // Scalar port indices start after the three fixed ports
        // (base, over, mask) — `mask` always occupies index 2 even
        // when unconnected, so eval's `inputs[2]` lookup stays valid.
        let mut r = InReader::new(fields, ctx, 3);
        let mode = r.enum_or("mode", BlendMode::Normal, MODES)?;
        let composite = if fields.contains_key("composite") {
            Some(r.enum_or("composite", Composite::SourceOver, COMPOSITES)?)
        } else {
            None
        };
        // `clip` predates the operator list; `clip: true` is source-atop.
        // It still works on its own, and gives way to `composite`.
        if let Some(clip) = fields.get("clip") {
            if composite.is_some() {
                ctx.warn("`clip` ignored: `composite` sets the operator");
            } else {
                match clip.as_bool() {
                    Some(true) => ctx.warn(
                        "`clip` is deprecated: write \"composite\": \"source-atop\" instead",
                    ),
                    Some(false) => ctx.warn(
                        "`clip` is deprecated: drop it (\"composite\": \"source-over\" is the default)",
                    ),
                    None => ctx.warn(
                        "`clip` is deprecated: write \"composite\": \"source-atop\" where it is true",
                    ),
                }
            }
        }
        let has_mask = mask.is_some();
        let opacity = r.number_or("opacity", 1.0)?;
        let clip = r.bool_or("clip", false)?;
        let parts = r.finish();

        let mut ports = vec![
            PortSpec {
                name: "base",
                accepts: ACCEPTS_RASTER_OR_SPRITE,
                optional: false,
            },
            PortSpec {
                name: "over",
                accepts: ACCEPTS_RASTER_OR_SPRITE,
                optional: false,
            },
            PortSpec {
                name: "mask",
                accepts: ACCEPTS_RASTER_OR_SPRITE,
                optional: true,
            },
        ];
        ports.extend(parts.ports);

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
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(BlendNode {
                mode,
                composite,
                opacity,
                clip,
                has_mask,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Composite `over` onto `base` with a W3C blend mode and a Porter-Duff operator (`composite`, default `source-over`). The mode mixes the colours where both are present; the operator decides how much of each survives, as in the W3C Compositing and Blending spec. `composite: \"source-atop\"` clips the result to base alpha (Photoshop-style clipping mask): the result *takes* the base's alpha, so clipping onto a 0.12-alpha wash caps the whole overlay at 0.12 — when you mean \"restrict this to an area\", give an opaque shape as `mask` instead. `composite: \"destination-out\"` makes `over` erase `base` (brush-eraser effect when `over` is a brush-shaped raster). Optional `mask` raster's alpha modulates source coverage.",
            "properties": {
                "base": schema_frag::node_ref(),
                "over": schema_frag::node_ref(),
                "mask": schema_frag::node_ref(),
                "mode": schema_frag::in_enum(serde_json::json!({
                    "type": "string",
                    "enum": enum_names(MODES),
                    "default": "normal"
                })),
                "composite": schema_frag::in_enum(serde_json::json!({
                    "type": "string",
                    "enum": enum_names(COMPOSITES),
                    "default": "source-over",
                    "description": "Porter-Duff operator, by its W3C Compositing name. `over`, `source`, and `dest-out` / `erase` are accepted for `source-over`, `copy` and `destination-out`."
                })),
                "clip": {
                    "oneOf": [{"type": "boolean"}, {"type": "string", "pattern": "^[$@].+"}],
                    "default": false,
                    "deprecated": true,
                    "description": "Deprecated: use `\"composite\": \"source-atop\"`, which is what `true` means. Ignored when `composite` is set."
                },
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Tiny deterministic LCG so tests need no `rand` dependency.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
            (self.0 >> 33) as u32
        }
        fn byte(&mut self) -> u8 {
            (self.next() & 0xff) as u8
        }
    }

    /// A random raster with valid premultiplied pixels (`rgb <= a`).
    fn random_premul(w: u32, h: u32, seed: u64) -> RasterBuf {
        let mut rng = Lcg(seed);
        let mut buf = RasterBuf::new(w, h);
        for px in buf.pixels.as_chunks_mut::<4>().0 {
            let a = rng.byte();
            px[0] = rng.byte().min(a);
            px[1] = rng.byte().min(a);
            px[2] = rng.byte().min(a);
            px[3] = a;
        }
        buf
    }

    fn assert_close(fast: &RasterBuf, reference: &RasterBuf, tol: i32) {
        assert_eq!(fast.pixels.len(), reference.pixels.len());
        for (i, (&f, &r)) in fast.pixels.iter().zip(reference.pixels.iter()).enumerate() {
            let d = (f as i32 - r as i32).abs();
            assert!(
                d <= tol,
                "byte {i}: fast={f} reference={r} diff={d} > {tol}"
            );
        }
    }

    #[test]
    fn normal_over_matches_general_opacity_1() {
        let base = random_premul(31, 17, 0x1234);
        let over = random_premul(31, 17, 0x9abc);
        let fast = normal_over(&base, &over, 1.0);
        let reference = blend_general(
            &base,
            &over,
            None,
            BlendMode::Normal,
            Composite::SourceOver,
            1.0,
        );
        assert_close(&fast, &reference, 1);
    }

    #[test]
    fn normal_over_matches_general_opacity_half() {
        let base = random_premul(31, 17, 0x5555);
        let over = random_premul(31, 17, 0xaaaa);
        let fast = normal_over(&base, &over, 0.5);
        let reference = blend_general(
            &base,
            &over,
            None,
            BlendMode::Normal,
            Composite::SourceOver,
            0.5,
        );
        assert_close(&fast, &reference, 1);
    }

    #[test]
    fn blank_over_is_base_exactly() {
        let base = random_premul(23, 29, 0xfeed);
        let over = RasterBuf::new(23, 29); // all zero = fully transparent
        assert!(over.is_blank());
        assert!(!base.is_blank());
        // General blend with a transparent `over` reproduces `base` byte for
        // byte — the property the eval fast path relies on.
        let out = blend_general(
            &base,
            &over,
            None,
            BlendMode::Normal,
            Composite::SourceOver,
            1.0,
        );
        assert_eq!(out.pixels, base.pixels);
    }

    /// One pixel each of a half-transparent red source and a
    /// three-quarter-opaque blue backdrop, composited with `c`.
    fn one_pixel(c: Composite, mode: BlendMode) -> [u8; 4] {
        let mut base = RasterBuf::new(1, 1);
        base.pixels.copy_from_slice(&[0, 0, 191, 191]);
        let mut over = RasterBuf::new(1, 1);
        over.pixels.copy_from_slice(&[128, 0, 0, 128]);
        let out = blend_general(&base, &over, None, mode, c, 1.0);
        [out.pixels[0], out.pixels[1], out.pixels[2], out.pixels[3]]
    }

    #[test]
    fn every_operator_follows_its_porter_duff_factors() {
        // αs = 128/255, αb = 191/255; premultiplied red cs = 128 and blue
        // cb = 191. With `normal` the blended source is the source itself,
        // so each channel is cs*Fa + cb*Fb.
        let sa = 128.0 / 255.0;
        let ba = 191.0 / 255.0;
        let cases = [
            (Composite::Clear, 0.0, 0.0),
            (Composite::Copy, 1.0, 0.0),
            (Composite::Destination, 0.0, 1.0),
            (Composite::SourceOver, 1.0, 1.0 - sa),
            (Composite::DestinationOver, 1.0 - ba, 1.0),
            (Composite::SourceIn, ba, 0.0),
            (Composite::DestinationIn, 0.0, sa),
            (Composite::SourceOut, 1.0 - ba, 0.0),
            (Composite::DestinationOut, 0.0, 1.0 - sa),
            (Composite::SourceAtop, ba, 1.0 - sa),
            (Composite::DestinationAtop, 1.0 - ba, sa),
            (Composite::Xor, 1.0 - ba, 1.0 - sa),
        ];
        for (c, fa, fb) in cases {
            let px = one_pixel(c, BlendMode::Normal);
            let want = [
                (128.0 * fa as f32).round() as i32,
                0,
                (191.0 * fb as f32).round() as i32,
                ((128.0 * fa + 191.0 * fb) as f32).round() as i32,
            ];
            for k in 0..4 {
                assert!(
                    (px[k] as i32 - want[k]).abs() <= 1,
                    "{c:?}: got {px:?}, want {want:?}"
                );
            }
        }
    }

    #[test]
    fn blend_mode_reaches_the_source_part_of_every_operator() {
        // Multiply of red onto blue is black where they overlap, so under
        // `source-in`, which keeps only the overlap, the source loses red.
        let normal = one_pixel(Composite::SourceIn, BlendMode::Normal);
        let multiplied = one_pixel(Composite::SourceIn, BlendMode::Multiply);
        assert_eq!(normal[3], multiplied[3], "the operator alone sets alpha");
        assert!(multiplied[0] < normal[0], "{multiplied:?} vs {normal:?}");
        // Under `destination-in` the source contributes no colour, so the
        // mode changes nothing.
        assert_eq!(
            one_pixel(Composite::DestinationIn, BlendMode::Normal),
            one_pixel(Composite::DestinationIn, BlendMode::Multiply),
        );
    }

    #[test]
    fn aliases_name_the_same_operators() {
        let parse = |s: &str| COMPOSITES.iter().find(|(n, _)| *n == s).map(|(_, c)| *c);
        assert_eq!(parse("over"), Some(Composite::SourceOver));
        assert_eq!(parse("source"), Some(Composite::Copy));
        assert_eq!(parse("dest-out"), Some(Composite::DestinationOut));
        assert_eq!(parse("erase"), Some(Composite::DestinationOut));
        assert_eq!(parse("plus"), None);
    }

    #[test]
    fn empty_source_keeps_base_exactly_where_the_operator_says_so() {
        let base = random_premul(9, 7, 0xbeef);
        let over = RasterBuf::new(9, 7);
        for c in [
            Composite::Clear,
            Composite::Copy,
            Composite::Destination,
            Composite::SourceOver,
            Composite::DestinationOver,
            Composite::SourceIn,
            Composite::DestinationIn,
            Composite::SourceOut,
            Composite::DestinationOut,
            Composite::SourceAtop,
            Composite::DestinationAtop,
            Composite::Xor,
        ] {
            let out = blend_general(&base, &over, None, BlendMode::Normal, c, 1.0);
            if c.keeps_base_under_empty_source() {
                // Within a rounding step: the eval fast path returns base
                // itself, which the general loop matches.
                assert_close(&out, &base, 1);
            } else {
                assert!(out.is_blank(), "{c:?} should clear under an empty source");
            }
        }
    }
}
