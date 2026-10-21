//! `dither` — `Raster -> Raster`. Reduce to a fixed `palette` with
//! dithering so gradients survive a small palette (retro / print / pixel-art
//! looks). `method: "floyd-steinberg"` (default) diffuses quantization error
//! to neighbouring pixels; `method: "ordered"` applies a 4×4 Bayer matrix.
//! Nearest-colour distance uses the same perceptual `space` as `quantize`.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EnumTable, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InEnum, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
    RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::raster::palette::{Palette, ResolvedPalette};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    FloydSteinberg,
    Ordered,
}

/// `fs` and `bayer` are shorter names for the two methods.
const METHODS: EnumTable<Method> = &[
    ("floyd-steinberg", Method::FloydSteinberg),
    ("ordered", Method::Ordered),
    ("fs", Method::FloydSteinberg),
    ("bayer", Method::Ordered),
];

// 4×4 Bayer threshold matrix, normalised to (value + 0.5)/16 in [0, 1).
#[rustfmt::skip]
const BAYER4: [f32; 16] = [
    0.0,  8.0,  2.0, 10.0,
    12.0, 4.0, 14.0,  6.0,
    3.0, 11.0,  1.0,  9.0,
    15.0, 7.0, 13.0,  5.0,
];

struct DitherNode {
    palette: Palette,
    method: InEnum<Method>,
    /// Ordered-dither strength, clamped to [0, 1] when resolved.
    amount: In<f64>,
    /// `input`, then a scalar port if `amount` is wired to one.
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for DitherNode {
    fn op_name(&self) -> &'static str {
        "dither"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let src = inputs[0]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let w = src.width as usize;
        let h = src.height as usize;
        // Resolved and re-projected once, outside the pixel loops.
        let palette = self.palette.resolve(ctx, inputs)?;
        let mut out = RasterBuf::new(src.width, src.height);
        match self.method.get(ctx)? {
            Method::Ordered => {
                let amount = self.amount.get(ctx, inputs)?.clamp(0.0, 1.0) as f32;
                let ratio = ctx.canvas.pixel_ratio;
                self.ordered(src, &palette, amount, ratio, w, h, &mut out)
            }
            Method::FloydSteinberg => self.floyd_steinberg(src, &palette, w, h, &mut out),
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"dither");
        self.method.param_hash(h);
        // A literal amount hashes as the clamped number it always did, so
        // existing styles keep their cache keys.
        match &self.amount {
            In::Const(v) => h.update(&(v.clamp(0.0, 1.0) as f32).to_le_bytes()),
            amount => amount.param_hash(h),
        }
        self.palette.hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

impl DitherNode {
    /// `ratio` is the canvas's pixel ratio: one cell of the Bayer matrix
    /// covers one style pixel, so a denser canvas draws the same pattern
    /// rather than a finer one.
    #[allow(clippy::too_many_arguments)]
    fn ordered(
        &self,
        src: &RasterBuf,
        palette: &ResolvedPalette,
        amount: f32,
        ratio: f32,
        w: usize,
        h: usize,
        out: &mut RasterBuf,
    ) {
        let cell = |v: usize| {
            if ratio == 1.0 {
                v % 4
            } else {
                (v as f64 / ratio as f64).floor() as usize % 4
            }
        };
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let a = src.pixels[i + 3] as f32 / 255.0;
                if a <= 0.0 {
                    continue;
                }
                let bias = (BAYER4[cell(y) * 4 + cell(x)] + 0.5) / 16.0 - 0.5;
                let d = bias * amount;
                let rgb = [
                    ((src.pixels[i] as f32 / 255.0 / a) + d).clamp(0.0, 1.0),
                    ((src.pixels[i + 1] as f32 / 255.0 / a) + d).clamp(0.0, 1.0),
                    ((src.pixels[i + 2] as f32 / 255.0 / a) + d).clamp(0.0, 1.0),
                ];
                write_premul(out, i, palette.nearest(rgb), a);
                out.pixels[i + 3] = src.pixels[i + 3];
            }
        }
    }

    fn floyd_steinberg(
        &self,
        src: &RasterBuf,
        palette: &ResolvedPalette,
        w: usize,
        h: usize,
        out: &mut RasterBuf,
    ) {
        // Working buffer of straight RGB; error is diffused into it.
        let mut buf = vec![[0f32; 3]; w * h];
        for (p, px) in buf.iter_mut().zip(src.pixels.as_chunks::<4>().0) {
            let a = px[3] as f32 / 255.0;
            *p = if a > 0.0 {
                [
                    px[0] as f32 / 255.0 / a,
                    px[1] as f32 / 255.0 / a,
                    px[2] as f32 / 255.0 / a,
                ]
            } else {
                [0.0; 3]
            };
        }
        for y in 0..h {
            for x in 0..w {
                let idx = y * w + x;
                let i = idx * 4;
                let a = src.pixels[i + 3] as f32 / 255.0;
                if a <= 0.0 {
                    continue; // transparent: no output, no diffusion
                }
                let old = [
                    buf[idx][0].clamp(0.0, 1.0),
                    buf[idx][1].clamp(0.0, 1.0),
                    buf[idx][2].clamp(0.0, 1.0),
                ];
                let q = palette.nearest(old);
                let err = [old[0] - q[0], old[1] - q[1], old[2] - q[2]];
                write_premul(out, i, q, a);
                out.pixels[i + 3] = src.pixels[i + 3];
                // Distribute error (7/16 →, 3/16 ↙, 5/16 ↓, 1/16 ↘).
                diffuse(&mut buf, w, h, x + 1, y, err, 7.0 / 16.0);
                if x > 0 {
                    diffuse(&mut buf, w, h, x - 1, y + 1, err, 3.0 / 16.0);
                }
                diffuse(&mut buf, w, h, x, y + 1, err, 5.0 / 16.0);
                diffuse(&mut buf, w, h, x + 1, y + 1, err, 1.0 / 16.0);
            }
        }
    }
}

fn diffuse(buf: &mut [[f32; 3]], w: usize, h: usize, x: usize, y: usize, err: [f32; 3], f: f32) {
    if x >= w || y >= h {
        return;
    }
    let p = &mut buf[y * w + x];
    p[0] += err[0] * f;
    p[1] += err[1] * f;
    p[2] += err[2] * f;
}

#[inline]
fn write_premul(out: &mut RasterBuf, i: usize, rgb: [f32; 3], a: f32) {
    out.pixels[i] = (rgb[0] * a * 255.0).round() as u8;
    out.pixels[i + 1] = (rgb[1] * a * 255.0).round() as u8;
    out.pixels[i + 2] = (rgb[2] * a * 255.0).round() as u8;
}

pub(super) struct DitherFactory;
impl NodeFactory for DitherFactory {
    fn op_name(&self) -> &'static str {
        "dither"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let mut r = InReader::new(fields, ctx, 1);
        let palette = Palette::from_fields(fields, &mut r)?;
        let method = r.enum_or("method", Method::FloydSteinberg, METHODS)?;
        let amount = r.number_or("amount", 0.5)?;
        let parts = r.finish();

        let mut ports = vec![PortSpec {
            name: "input",
            accepts: &[PortKind::Raster],
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "input".into(),
            src: input,
        }];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(DitherNode {
                palette,
                method,
                amount,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Reduce to a fixed `palette` with dithering so gradients survive a small palette. `method: \"floyd-steinberg\"` (default) diffuses error to neighbours; `method: \"ordered\"` uses a 4×4 Bayer matrix with strength `amount` (0..1). Nearest-colour distance uses `space` (`lab` default / `rgb`). Alpha preserved.",
            "properties": {
                "input": schema_frag::node_ref(),
                "palette": {
                    "type": "array",
                    "minItems": 1,
                    "items": schema_frag::nested_color(),
                    "description": "Each entry is `#rrggbb` / `#rrggbbaa` (alpha ignored for matching) or a `$param`, so a palette can be recoloured at render time.",
                },
                "method": schema_frag::in_enum(serde_json::json!({ "type": "string", "enum": ["floyd-steinberg", "ordered"], "default": "floyd-steinberg" })),
                "amount": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.5, "description": "Ordered-dither strength (ignored for floyd-steinberg)." })),
                "space": schema_frag::in_enum(serde_json::json!({ "type": "string", "enum": ["lab", "rgb"], "default": "lab" })),
            },
            "required": ["input", "palette"],
        })
    }
}

ezu_graph::submit_node!(DitherFactory);
