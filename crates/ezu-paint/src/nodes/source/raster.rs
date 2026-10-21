//! `raster` — `() -> Raster`. Resolves a host-bound RGBA tile mosaic
//! (satellite imagery, pre-rendered basemaps, …) via the unified
//! [`AssetLoader`](ezu_graph::AssetLoader) and emits it as a
//! canvas-padded `Raster` for downstream filters (`posterize`, `hsl`,
//! `blend`, …).
//!
//! The host declares the underlying tile pyramid in the document's
//! `sources` block (`type: "raster"`), fetches + stitches the 3×3
//! neighbourhood, and binds the padded buffer under the source's bare
//! name via `TileLoader::bind_raster` before each render. The style's
//! `raster` node references it via `source: "<name>"`; omitting the
//! field falls back to the document's only `raster` source and warns.
//! An
//! unbound source (404 with `on-missing: empty`) emits transparent
//! pixels, and so does a tile outside the node's `min-zoom`/`max-zoom`.
//!
//! The node can also adjust the imagery as it reads it: `opacity`,
//! `hue-rotate-deg`, `saturation`, `contrast`, `brightness-min` and
//! `brightness-max` follow MapLibre's raster layer to the formula, so a
//! translated raster layer looks as it does there. Left at their
//! defaults they change nothing and the bound pixels pass through as
//! they are.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, Asset, AssetError, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, In,
    InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{read_optional_zoom, resolve_source};

struct RasterNode {
    name: String,
    min_zoom: Option<u8>,
    max_zoom: Option<u8>,
    opacity: In<f64>,
    hue_rotate_deg: In<f64>,
    saturation: In<f64>,
    contrast: In<f64>,
    brightness_min: In<f64>,
    brightness_max: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

/// What an absent source draws: transparent pixels over the padded canvas.
fn transparent(ctx: &EvalCtx<'_>) -> PortValue {
    let (pw, ph) = ctx.canvas.padded_dims();
    PortValue::Raster(Arc::new(RasterBuf::new(pw, ph)))
}

/// The resolved colour adjustment for one render, held as the factors
/// MapLibre's raster shader receives, so the per-pixel work is the
/// shader's own arithmetic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RasterAdjust {
    pub opacity: f32,
    /// The three weights of the hue-rotation matrix, whose rows are these
    /// weights in turn rotated (`xyz`, `zxy`, `yzx`).
    pub spin: [f32; 3],
    pub saturation: f32,
    pub contrast: f32,
    pub brightness_low: f32,
    pub brightness_high: f32,
}

impl RasterAdjust {
    /// From the style values: opacity in `[0, 1]`, hue rotation in
    /// degrees, saturation and contrast in `[-1, 1]`, and the brightness
    /// range in `[0, 1]`. The factors are worked out in `f64` and handed
    /// on as `f32`, as MapLibre computes its uniforms in JavaScript and
    /// the GPU reads them as floats.
    pub fn new(
        opacity: f64,
        hue_rotate_deg: f64,
        saturation: f64,
        contrast: f64,
        brightness_min: f64,
        brightness_max: f64,
    ) -> Self {
        let angle = hue_rotate_deg.to_radians();
        let (s, c) = (libm::sin(angle), libm::cos(angle));
        let root3 = 3f64.sqrt();
        let spin = [
            (2.0 * c + 1.0) / 3.0,
            (-root3 * s - c + 1.0) / 3.0,
            (root3 * s - c + 1.0) / 3.0,
        ];
        // Saturation and contrast are not linear in their style values:
        // above zero each grows hyperbolically, so `1` means "as far as
        // it goes" (MapLibre keeps saturation's pole just past 1).
        let saturation = if saturation > 0.0 {
            1.0 - 1.0 / (1.001 - saturation)
        } else {
            -saturation
        };
        let contrast = if contrast > 0.0 {
            1.0 / (1.0 - contrast)
        } else {
            1.0 + contrast
        };
        Self {
            opacity: opacity.clamp(0.0, 1.0) as f32,
            spin: spin.map(|w| w as f32),
            saturation: saturation as f32,
            contrast: contrast as f32,
            brightness_low: brightness_min as f32,
            brightness_high: brightness_max as f32,
        }
    }

    /// Whether the adjustment leaves every pixel as it is, so the bound
    /// buffer can pass through untouched.
    fn is_identity(opacity: f64, hue: f64, sat: f64, con: f64, bmin: f64, bmax: f64) -> bool {
        opacity >= 1.0 && hue == 0.0 && sat == 0.0 && con == 0.0 && bmin == 0.0 && bmax == 1.0
    }

    /// Adjust one premultiplied RGBA8 pixel, step for step as MapLibre's
    /// raster fragment shader does: unpremultiply, scale alpha by the
    /// opacity, rotate the hue, pull towards or push away from the grey of
    /// the pixel's mean, stretch around mid-grey, and map `0..1` onto the
    /// brightness range. Nothing is clamped until the colour is written,
    /// as on the GPU.
    pub fn apply(&self, px: [u8; 4]) -> [u8; 4] {
        let a0 = px[3] as f32 / 255.0;
        if a0 <= 0.0 {
            return [0; 4];
        }
        let mut c = [px[0], px[1], px[2]].map(|v| v as f32 / 255.0 / a0);
        let alpha = a0 * self.opacity;
        // The mean is taken before the hue turns. The rotation keeps the
        // sum of the channels, so this matches the rotated colour's mean.
        let average = (c[0] + c[1] + c[2]) / 3.0;
        let w = self.spin;
        let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        c = [
            dot(c, [w[0], w[1], w[2]]),
            dot(c, [w[2], w[0], w[1]]),
            dot(c, [w[1], w[2], w[0]]),
        ];
        let (lo, hi) = (self.brightness_low, self.brightness_high);
        let out = c.map(|v| {
            let v = v + (average - v) * self.saturation;
            let v = (v - 0.5) * self.contrast + 0.5;
            // GLSL `mix(lo, hi, v)`.
            lo * (1.0 - v) + hi * v
        });
        let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        [
            to_u8(out[0] * alpha),
            to_u8(out[1] * alpha),
            to_u8(out[2] * alpha),
            to_u8(alpha),
        ]
    }
}

impl RasterNode {
    fn adjust(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<Option<RasterAdjust>, EvalError> {
        let opacity = self.opacity.get(ctx, inputs)?;
        let hue = self.hue_rotate_deg.get(ctx, inputs)?;
        let sat = self.saturation.get(ctx, inputs)?;
        let con = self.contrast.get(ctx, inputs)?;
        let bmin = self.brightness_min.get(ctx, inputs)?;
        let bmax = self.brightness_max.get(ctx, inputs)?;
        if RasterAdjust::is_identity(opacity, hue, sat, con, bmin, bmax) {
            return Ok(None);
        }
        Ok(Some(RasterAdjust::new(opacity, hue, sat, con, bmin, bmax)))
    }
}

impl Node for RasterNode {
    fn op_name(&self) -> &'static str {
        "raster"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn asset_inputs(&self) -> Vec<String> {
        vec![self.name.clone()]
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        // Outside the node's zoom band it draws what an absent source
        // does, without looking the binding up — as `features` does.
        let z = ctx.tile.z;
        if self.min_zoom.is_some_and(|mn| z < mn) || self.max_zoom.is_some_and(|mx| z > mx) {
            return Ok(transparent(ctx));
        }
        let asset = match ctx.assets.load(&self.name) {
            Ok(a) => a,
            // No binding for this tile -> transparent canvas.
            Err(AssetError::NotFound(_)) => return Ok(transparent(ctx)),
            Err(e) => return Err(EvalError::Asset(e)),
        };
        let Asset::Image(buf) = asset else {
            return Err(EvalError::Other(format!(
                "asset `{}` is not an image",
                self.name
            )));
        };
        let (pw, ph) = ctx.canvas.padded_dims();
        if buf.width != pw || buf.height != ph {
            return Err(EvalError::Other(format!(
                "raster source `{}`: bound buffer is {}x{}, expected padded canvas {pw}x{ph}",
                self.name, buf.width, buf.height
            )));
        }
        let Some(adjust) = self.adjust(ctx, inputs)? else {
            return Ok(PortValue::Raster(buf));
        };
        let mut out = RasterBuf::new(buf.width, buf.height);
        for (dst, src) in out
            .pixels
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(buf.pixels.as_chunks::<4>().0)
        {
            *dst = adjust.apply(*src);
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"raster");
        h.update(self.name.as_bytes());
        if let Some(z) = self.min_zoom {
            h.update(b"minz");
            h.update(&[z]);
        }
        if let Some(z) = self.max_zoom {
            h.update(b"maxz");
            h.update(&[z]);
        }
        self.opacity.param_hash(h);
        self.hue_rotate_deg.param_hash(h);
        self.saturation.param_hash(h);
        self.contrast.param_hash(h);
        self.brightness_min.param_hash(h);
        self.brightness_max.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct RasterFactory;
impl NodeFactory for RasterFactory {
    fn op_name(&self) -> &'static str {
        "raster"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let name = resolve_raster_source(fields, ctx)?;
        let mut r = InReader::new(fields, ctx, 0);
        let opacity = r.number_or("opacity", 1.0)?;
        let hue_rotate_deg = r.number_or("hue-rotate-deg", 0.0)?;
        let saturation = r.number_or("saturation", 0.0)?;
        let contrast = r.number_or("contrast", 0.0)?;
        let brightness_min = r.number_or("brightness-min", 0.0)?;
        let brightness_max = r.number_or("brightness-max", 1.0)?;
        let parts = r.finish();
        Ok(BuiltNode {
            node: Box::new(RasterNode {
                name,
                min_zoom: read_optional_zoom(fields, "min-zoom")?,
                max_zoom: read_optional_zoom(fields, "max-zoom")?,
                opacity,
                hue_rotate_deg,
                saturation,
                contrast,
                brightness_min,
                brightness_max,
                ports: parts.ports,
                param_refs: parts.param_refs,
            }),
            connections: parts.connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Sample a host-bound RGBA tile pyramid (satellite imagery, pre-rendered basemaps) as a canvas-padded Raster. `source` names a `raster` entry in the document's `sources` block; omitting it falls back to the document's only `raster` source, with a build warning. The colour fields adjust the imagery with the formulas of MapLibre's raster layer, applied in its order: hue, saturation, contrast, then the brightness range. At their defaults the pixels pass through unchanged.",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "Name of a `raster` source in the document's `sources` block. Omitting it resolves to the only such source and warns at build time."
                },
                "min-zoom": { "type": "integer", "minimum": 0, "maximum": 24,
                              "description": "Style-level minimum zoom. Below this zoom the node emits transparent pixels, as for a tile the source does not have." },
                "max-zoom": { "type": "integer", "minimum": 0, "maximum": 24,
                              "description": "Style-level maximum zoom. Above this zoom the node emits transparent pixels." },
                "opacity": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 1.0,
                              "description": "Multiplies the imagery's alpha (MapLibre's `raster-opacity`)." })),
                "hue-rotate-deg": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.0,
                              "description": "Turns every hue by this many degrees around the grey axis, keeping each pixel's channel sum (MapLibre's `raster-hue-rotate`)." })),
                "saturation": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": -1.0, "maximum": 1.0, "default": 0.0,
                              "description": "Moves each channel towards (below 0) or away from (above 0) the mean of the pixel's channels. -1 is grey; towards 1 the push grows without bound (MapLibre's `raster-saturation`)." })),
                "contrast": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": -1.0, "maximum": 1.0, "default": 0.0,
                              "description": "Scales each channel around mid-grey: by `1 + contrast` below 0, by `1 / (1 - contrast)` above it (MapLibre's `raster-contrast`)." })),
                "brightness-min": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.0,
                              "description": "What a channel at 0 becomes; the range `0..1` is mapped linearly onto `brightness-min..brightness-max` (MapLibre's `raster-brightness-min`)." })),
                "brightness-max": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 1.0,
                              "description": "What a channel at 1 becomes (MapLibre's `raster-brightness-max`)." }))
            },
        })
    }
}

fn resolve_raster_source(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
) -> Result<String, FactoryError> {
    resolve_source(fields, ctx, "`raster`", |decl| {
        matches!(decl, ezu_style::SourceDecl::Raster(_))
    })
}

ezu_graph::submit_node!(RasterFactory);

#[cfg(test)]
mod tests {
    use super::RasterAdjust;

    /// MapLibre's raster shader written out again in `f64`, straight from
    /// `raster.fragment.glsl` and the uniforms of `raster_program.ts`, to
    /// hold the `f32` implementation to.
    fn maplibre(px: [u8; 4], o: f64, hue: f64, sat: f64, con: f64, lo: f64, hi: f64) -> [f64; 4] {
        let a0 = px[3] as f64 / 255.0;
        let mut c = [px[0], px[1], px[2]].map(|v| v as f64 / 255.0 / a0);
        let a = a0 * o;
        let t = hue * std::f64::consts::PI / 180.0;
        let (s, cs) = (libm::sin(t), libm::cos(t));
        let w = [
            (2.0 * cs + 1.0) / 3.0,
            (-(3f64.sqrt()) * s - cs + 1.0) / 3.0,
            (3f64.sqrt() * s - cs + 1.0) / 3.0,
        ];
        let avg = (c[0] + c[1] + c[2]) / 3.0;
        c = [
            c[0] * w[0] + c[1] * w[1] + c[2] * w[2],
            c[0] * w[2] + c[1] * w[0] + c[2] * w[1],
            c[0] * w[1] + c[1] * w[2] + c[2] * w[0],
        ];
        let sf = if sat > 0.0 {
            1.0 - 1.0 / (1.001 - sat)
        } else {
            -sat
        };
        let cf = if con > 0.0 {
            1.0 / (1.0 - con)
        } else {
            1.0 + con
        };
        let m = c.map(|v| {
            let v = v + (avg - v) * sf;
            let v = (v - 0.5) * cf + 0.5;
            lo * (1.0 - v) + hi * v
        });
        [m[0] * a, m[1] * a, m[2] * a, a].map(|v| v.clamp(0.0, 1.0) * 255.0)
    }

    fn check(px: [u8; 4], o: f64, hue: f64, sat: f64, con: f64, lo: f64, hi: f64) {
        let got = RasterAdjust::new(o, hue, sat, con, lo, hi).apply(px);
        let want = maplibre(px, o, hue, sat, con, lo, hi);
        for ch in 0..4 {
            assert!(
                (got[ch] as f64 - want[ch]).abs() <= 0.5 + 1e-3,
                "{px:?} o{o} h{hue} s{sat} c{con} b{lo}..{hi}: channel {ch} got {got:?}, want {want:?}"
            );
        }
    }

    const SAMPLES: [[u8; 4]; 6] = [
        [200, 100, 50, 255],
        [255, 0, 0, 255],
        [10, 200, 120, 255],
        [60, 30, 90, 128],
        [128, 128, 128, 255],
        [0, 0, 0, 255],
    ];

    #[test]
    fn opacity_scales_alpha_and_premultiplied_colour() {
        assert_eq!(
            RasterAdjust::new(0.5, 0.0, 0.0, 0.0, 0.0, 1.0).apply([200, 100, 50, 255]),
            [100, 50, 25, 128]
        );
        for px in SAMPLES {
            check(px, 0.3, 0.0, 0.0, 0.0, 0.0, 1.0);
        }
    }

    #[test]
    fn hue_rotate_turns_red_to_green_at_120_degrees() {
        // At 120° the weights are (0, 0, 1): each channel takes the one
        // before it, so red becomes green.
        assert_eq!(
            RasterAdjust::new(1.0, 120.0, 0.0, 0.0, 0.0, 1.0).apply([255, 0, 0, 255]),
            [0, 255, 0, 255]
        );
        for hue in [-90.0, 33.0, 180.0, 300.0] {
            for px in SAMPLES {
                check(px, 1.0, hue, 0.0, 0.0, 0.0, 1.0);
            }
        }
    }

    #[test]
    fn saturation_minus_one_is_the_channel_mean() {
        assert_eq!(
            RasterAdjust::new(1.0, 0.0, -1.0, 0.0, 0.0, 1.0).apply([255, 0, 0, 255]),
            [85, 85, 85, 255]
        );
        for sat in [-0.6, 0.3, 0.8, 1.0] {
            for px in SAMPLES {
                check(px, 1.0, 0.0, sat, 0.0, 0.0, 1.0);
            }
        }
    }

    #[test]
    fn contrast_scales_around_mid_grey() {
        // +0.5 doubles the distance from 0.5; -0.5 halves it.
        let up = RasterAdjust::new(1.0, 0.0, 0.0, 0.5, 0.0, 1.0);
        assert_eq!(up.apply([192, 63, 255, 255]), [255, 0, 255, 255]);
        let down = RasterAdjust::new(1.0, 0.0, 0.0, -0.5, 0.0, 1.0);
        assert_eq!(down.apply([255, 0, 0, 255]), [191, 64, 64, 255]);
        for con in [-1.0, -0.3, 0.2, 0.9] {
            for px in SAMPLES {
                check(px, 1.0, 0.0, 0.0, con, 0.0, 1.0);
            }
        }
    }

    #[test]
    fn brightness_maps_the_unit_range_onto_min_max() {
        let b = RasterAdjust::new(1.0, 0.0, 0.0, 0.0, 0.2, 0.8);
        assert_eq!(b.apply([0, 255, 0, 255]), [51, 204, 51, 255]);
        // Swapped bounds invert the image.
        let inv = RasterAdjust::new(1.0, 0.0, 0.0, 0.0, 1.0, 0.0);
        assert_eq!(inv.apply([0, 255, 64, 255]), [255, 0, 191, 255]);
        for px in SAMPLES {
            check(px, 1.0, 0.0, 0.0, 0.0, 0.25, 0.6);
        }
    }

    #[test]
    fn all_together_follow_the_shader_order() {
        for px in SAMPLES {
            check(px, 0.7, 45.0, -0.4, 0.3, 0.1, 0.9);
            check(px, 1.0, 200.0, 0.5, -0.2, 0.0, 0.7);
        }
    }

    #[test]
    fn transparent_pixels_stay_transparent() {
        let all = RasterAdjust::new(0.5, 90.0, 0.5, 0.5, 0.3, 0.6);
        assert_eq!(all.apply([0, 0, 0, 0]), [0; 4]);
    }
}
