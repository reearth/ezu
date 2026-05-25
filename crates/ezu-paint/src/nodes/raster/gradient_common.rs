//! Shared helpers for `gradient-*` nodes.

use std::sync::Arc;

use ezu_graph::{EvalCtx, PortValue, RasterBuf};

use crate::nodes::common::Anchor;
use crate::nodes::raster::generator_kind::GeneratorKind;

/// Map a padded-canvas pixel to a "user-space" `(ux, uy)` coordinate
/// that the gradient's parameters are expressed in. Raster mode only.
///
/// - `Tile` anchor: tile spans `[0, 1] × [0, 1]`. Padding pixels
///   extend slightly outside that range, which is intentional — the
///   gradient continues into the padded border consistently.
/// - `World` anchor: the full Mercator world at z=0 spans `[0, 1] ×
///   [0, 1]`, with `(0, 0)` at the top-left. World-anchored gradients
///   stay continuous across tile borders.
#[inline]
pub(super) fn pixel_to_user(x: u32, y: u32, ctx: &EvalCtx<'_>, anchor: Anchor) -> (f32, f32) {
    let pad = ctx.canvas.pad as f32;
    let ts = ctx.canvas.tile_size as f32;
    let tx = (x as f32 + 0.5 - pad) / ts;
    let ty = (y as f32 + 0.5 - pad) / ts;
    match anchor {
        Anchor::Tile => (tx, ty),
        Anchor::World => {
            let zscale = (1u64 << ctx.tile.z) as f32;
            (
                (ctx.tile.x as f32 + tx) / zscale,
                (ctx.tile.y as f32 + ty) / zscale,
            )
        }
    }
}

/// Render a gradient into a buffer sized according to `out_kind`,
/// invoking `sample` with each pixel's user-space `(ux, uy)`. Returns
/// either a `Raster` or a `Sprite` `PortValue` matching the requested
/// kind.
///
/// In `Sprite` mode the `anchor` is ignored — sprite coordinates are
/// just `(x + 0.5)/width, (y + 0.5)/height`, so the gradient is
/// self-contained in the sprite. In `Raster` mode the existing
/// `pixel_to_user` mapping is applied.
pub(super) fn render_gradient<F>(
    ctx: &EvalCtx<'_>,
    out_kind: GeneratorKind,
    anchor: Anchor,
    sample: F,
) -> PortValue
where
    F: Fn(f32, f32) -> [f32; 4],
{
    let (w, h, raster_kind) = match out_kind {
        GeneratorKind::Raster => {
            let s = ctx.canvas.padded_size();
            (s, s, true)
        }
        GeneratorKind::Sprite { width, height } => (width, height, false),
    };
    let mut buf = RasterBuf::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (ux, uy) = if raster_kind {
                pixel_to_user(x, y, ctx, anchor)
            } else {
                ((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32)
            };
            let c = sample(ux, uy);
            let rgba = premul_u8(c);
            let i = ((y * w + x) * 4) as usize;
            buf.pixels[i..i + 4].copy_from_slice(&rgba);
        }
    }
    let buf = Arc::new(buf);
    if raster_kind {
        PortValue::Raster(buf)
    } else {
        PortValue::Sprite(buf)
    }
}

#[inline]
pub(super) fn premul_u8(c: [f32; 4]) -> [u8; 4] {
    let a = c[3].clamp(0.0, 1.0);
    [
        (c[0].clamp(0.0, 1.0) * a * 255.0).round() as u8,
        (c[1].clamp(0.0, 1.0) * a * 255.0).round() as u8,
        (c[2].clamp(0.0, 1.0) * a * 255.0).round() as u8,
        (a * 255.0).round() as u8,
    ]
}
