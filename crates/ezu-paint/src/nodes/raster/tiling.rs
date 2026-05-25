//! `tiling` — `Raster|Sprite -> Raster`. Repeat a source across the
//! canvas with wrap-around sampling. The primary use case is **paper
//! / canvas texture**: load a small PNG via `image`, tile it across
//! every map tile such that the pattern is continuous across tile
//! seams.
//!
//! Sampling is bilinear with wrap-around (`rem_euclid`) on both axes,
//! so the source image is interpreted as an infinite torus. With
//! `anchor: "world"` the texture is anchored to z=0 world pixels, so
//! adjacent tiles see a continuous pattern. With `anchor: "tile"` each
//! map tile gets its own copy starting at the tile's top-left.
//!
//! This node does **not** implement `cover` / `contain` style fitting
//! — that's a different concern (stretch to fill). For a single,
//! unrepeated image use `image` directly with `stamp`, or build a
//! separate fit-style node.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    read_number_or, read_optional_string, read_xy, unwrap_raster_or_sprite, Anchor,
    ACCEPTS_RASTER_OR_SPRITE,
};

struct TilingNode {
    anchor: Anchor,
    /// `None` → use the source raster's width as the natural period.
    scale_px: Option<f64>,
    offset_px: [f64; 2],
    rotation_deg: f64,
    opacity: f32,
}

impl Node for TilingNode {
    fn op_name(&self) -> &'static str {
        "tiling"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "input",
            accepts: ACCEPTS_RASTER_OR_SPRITE,
            optional: false,
        }];
        SPECS
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn coord_space(&self) -> CoordSpace {
        match self.anchor {
            Anchor::World => CoordSpace::World,
            Anchor::Tile => CoordSpace::Tile,
        }
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let (src, _) = unwrap_raster_or_sprite(input, "input")?;
        let size = ctx.canvas.padded_size();
        let pad = ctx.canvas.pad as f64;
        let tile_size = ctx.canvas.tile_size as f64;
        if src.width == 0 || src.height == 0 {
            return Ok(PortValue::Raster(Arc::new(RasterBuf::new(size, size))));
        }

        let (origin_x, origin_y) = match self.anchor {
            Anchor::World => (ctx.tile.x as f64 * tile_size, ctx.tile.y as f64 * tile_size),
            Anchor::Tile => (0.0, 0.0),
        };

        let scale = self.scale_px.unwrap_or(src.width as f64).max(0.5);
        // Source pixels span [0, src.w/h); one full repetition covers
        // `scale` canvas pixels. So canvas_px / scale * src.w gives the
        // source-pixel coordinate before wraparound.
        let kx = (src.width as f64) / scale;
        let ky = (src.height as f64) / scale;

        // Rotation maps canvas-space back to pattern-space, so the
        // sampler uses `-rotation_deg`. Rotating the pattern itself
        // visually clockwise corresponds to sampling counterclockwise.
        let theta = -self.rotation_deg.to_radians();
        let (sin_t, cos_t) = theta.sin_cos();
        let needs_rotation = self.rotation_deg.abs() > 1e-9;

        let sw = src.width as f64;
        let sh = src.height as f64;
        let src_w = src.width as usize;
        let src_h = src.height as usize;
        let opacity = self.opacity.clamp(0.0, 1.0);

        let mut out = RasterBuf::new(size, size);
        let dst = &mut out.pixels;
        let src_px = &src.pixels;
        for y in 0..size {
            for x in 0..size {
                let cx = origin_x + x as f64 - pad - self.offset_px[0];
                let cy = origin_y + y as f64 - pad - self.offset_px[1];
                let (rx, ry) = if needs_rotation {
                    (cx * cos_t - cy * sin_t, cx * sin_t + cy * cos_t)
                } else {
                    (cx, cy)
                };
                let sx = (rx * kx).rem_euclid(sw);
                let sy = (ry * ky).rem_euclid(sh);
                let pixel = bilinear_wrap(src_px, src_w, src_h, sx, sy);
                let i = ((y * size + x) * 4) as usize;
                if (opacity - 1.0).abs() < 1e-6 {
                    dst[i] = pixel[0];
                    dst[i + 1] = pixel[1];
                    dst[i + 2] = pixel[2];
                    dst[i + 3] = pixel[3];
                } else {
                    // RasterBuf is premultiplied — opacity scales every channel.
                    dst[i] = ((pixel[0] as f32) * opacity).round() as u8;
                    dst[i + 1] = ((pixel[1] as f32) * opacity).round() as u8;
                    dst[i + 2] = ((pixel[2] as f32) * opacity).round() as u8;
                    dst[i + 3] = ((pixel[3] as f32) * opacity).round() as u8;
                }
            }
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"tiling");
        match self.anchor {
            Anchor::World => h.update(b"w"),
            Anchor::Tile => h.update(b"t"),
        }
        match self.scale_px {
            Some(s) => {
                h.update(&[1]);
                h.update(&s.to_le_bytes());
            }
            None => h.update(&[0]),
        }
        h.update(&self.offset_px[0].to_le_bytes());
        h.update(&self.offset_px[1].to_le_bytes());
        h.update(&self.rotation_deg.to_le_bytes());
        h.update(&self.opacity.to_le_bytes());
    }
}

/// Bilinear sample on a premultiplied-RGBA8 buffer with wrap-around
/// addressing on both axes. `sx`, `sy` are in source-pixel units and
/// must already be reduced into `[0, w)` / `[0, h)` by the caller.
#[inline]
fn bilinear_wrap(pixels: &[u8], w: usize, h: usize, sx: f64, sy: f64) -> [u8; 4] {
    let x0 = sx.floor() as isize;
    let y0 = sy.floor() as isize;
    let fx = (sx - x0 as f64) as f32;
    let fy = (sy - y0 as f64) as f32;
    let x0u = ((x0).rem_euclid(w as isize)) as usize;
    let y0u = ((y0).rem_euclid(h as isize)) as usize;
    let x1u = if x0u + 1 == w { 0 } else { x0u + 1 };
    let y1u = if y0u + 1 == h { 0 } else { y0u + 1 };
    let p00 = pixel_at(pixels, w, x0u, y0u);
    let p10 = pixel_at(pixels, w, x1u, y0u);
    let p01 = pixel_at(pixels, w, x0u, y1u);
    let p11 = pixel_at(pixels, w, x1u, y1u);
    let w00 = (1.0 - fx) * (1.0 - fy);
    let w10 = fx * (1.0 - fy);
    let w01 = (1.0 - fx) * fy;
    let w11 = fx * fy;
    let mut out = [0u8; 4];
    for c in 0..4 {
        let v =
            p00[c] as f32 * w00 + p10[c] as f32 * w10 + p01[c] as f32 * w01 + p11[c] as f32 * w11;
        out[c] = v.round().clamp(0.0, 255.0) as u8;
    }
    out
}

#[inline]
fn pixel_at(pixels: &[u8], w: usize, x: usize, y: usize) -> [u8; 4] {
    let i = (y * w + x) * 4;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

pub(super) struct TilingFactory;
impl NodeFactory for TilingFactory {
    fn op_name(&self) -> &'static str {
        "tiling"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let anchor = match read_optional_string(fields, "anchor")?.as_deref() {
            None | Some("world") => Anchor::World,
            Some("tile") => Anchor::Tile,
            Some(other) => {
                return Err(FactoryError::BadField {
                    field: "anchor".into(),
                    msg: format!("expected `world` or `tile`, got `{other}`"),
                });
            }
        };
        let scale_px = if fields.contains_key("scale-px") {
            Some(read_number_or(fields, "scale-px", ctx, 0.0)?)
        } else {
            None
        };
        let offset = read_xy(fields, "offset-px", ctx, [0.0, 0.0])?;
        let offset_px = [offset[0] as f64, offset[1] as f64];
        let rotation_deg = read_number_or(fields, "rotation-deg", ctx, 0.0)?;
        let opacity = read_number_or(fields, "opacity", ctx, 1.0)? as f32;
        Ok(BuiltNode {
            node: Box::new(TilingNode {
                anchor,
                scale_px,
                offset_px,
                rotation_deg,
                opacity,
            }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Repeat a raster across the canvas with bilinear wrap-around sampling. Primary use case is paper / canvas texture composition. Does not stretch-to-fit — use a dedicated `cover`/`contain` node if that's what you want.",
            "properties": {
                "input": schema_frag::node_ref(),
                "anchor": { "type": "string", "enum": ["world", "tile"], "default": "world",
                            "description": "`world` (default) makes the pattern seamless across map tiles; `tile` restarts the pattern at every map tile's top-left." },
                "scale-px": { "type": "number", "minimum": 0.5,
                              "description": "Canvas pixels per one source repetition. Defaults to the source image's native width." },
                "offset-px": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2,
                               "description": "Pattern offset in canvas pixels [x, y]." },
                "rotation-deg": { "type": "number",
                                  "description": "Rotation of the pattern in degrees (clockwise)." },
                "opacity": schema_frag::unit_number(),
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(TilingFactory);
