//! Shared helpers for `gradient-*` nodes.

use std::sync::Arc;

use ezu_graph::{EvalCtx, FactoryCtx, FactoryError, InReader, PortValue, RasterBuf, ScalarField};
use serde_json::Value;

use crate::nodes::common::{read_stops, Anchor, StopsIn};
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
    // Per axis, so a gradient spans the canvas it is drawn on rather
    // than running off a canvas that is not square.
    let tx = (x as f32 + 0.5 - pad) / ctx.canvas.tile_w as f32;
    let ty = (y as f32 + 0.5 - pad) / ctx.canvas.tile_h as f32;
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

/// Render a gradient into a buffer sized according to `out_kind`.
/// `position` maps each pixel's user-space `(ux, uy)` to the gradient
/// parameter `t`, and `color` maps `t` through the stops. Returns a
/// `Raster`, a `Sprite` or a `ScalarField` matching the requested kind.
///
/// In `Sprite` mode the `anchor` is ignored — sprite coordinates are
/// just `(x + 0.5)/width, (y + 0.5)/height`, so the gradient is
/// self-contained in the sprite. In `Raster` and `Scalar` mode the
/// existing `pixel_to_user` mapping is applied. `Scalar` mode never calls
/// `color`: each sample is `t` clamped to `[0, 1]`, how far along the
/// gradient the pixel lies, before any stop is looked at.
pub(super) fn render_gradient<P, C>(
    ctx: &EvalCtx<'_>,
    out_kind: GeneratorKind,
    anchor: Anchor,
    position: P,
    color: C,
) -> PortValue
where
    P: Fn(f32, f32) -> f32,
    C: Fn(f32) -> [f32; 4],
{
    let (w, h) = match out_kind {
        GeneratorKind::Raster | GeneratorKind::Scalar => ctx.canvas.padded_dims(),
        GeneratorKind::Sprite { width, height } => (width, height),
    };
    let user = |x: u32, y: u32| match out_kind {
        GeneratorKind::Sprite { .. } => ((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32),
        _ => pixel_to_user(x, y, ctx, anchor),
    };
    if out_kind == GeneratorKind::Scalar {
        let mut values = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                let (ux, uy) = user(x, y);
                values.push(position(ux, uy).clamp(0.0, 1.0));
            }
        }
        return PortValue::ScalarField(Arc::new(ScalarField {
            width: w,
            height: h,
            values: values.into(),
            nodata: None,
            geo_scale: None,
        }));
    }
    let mut buf = RasterBuf::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (ux, uy) = user(x, y);
            let c = color(position(ux, uy));
            let rgba = premul_u8(c);
            let i = ((y * w + x) * 4) as usize;
            buf.pixels[i..i + 4].copy_from_slice(&rgba);
        }
    }
    let buf = Arc::new(buf);
    match out_kind {
        GeneratorKind::Sprite { .. } => PortValue::Sprite(buf),
        _ => PortValue::Raster(buf),
    }
}

/// Read a gradient's `stops`, which a `kind: scalar` gradient does
/// without: it gives each pixel's position along the gradient, not a
/// colour, so stops given anyway are ignored with a warning.
pub(super) fn read_gradient_stops(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
    r: &mut InReader<'_, '_>,
    out_kind: GeneratorKind,
) -> Result<StopsIn, FactoryError> {
    if out_kind == GeneratorKind::Scalar {
        if fields.contains_key("stops") {
            ctx.warn(
                "`stops` ignored: `kind: scalar` gives each pixel's position along the \
                 gradient, not a colour",
            );
        }
        return Ok(Vec::new());
    }
    read_stops(fields, "stops", r)
}

/// The schema's way of saying `stops` is required unless `kind` is
/// `scalar`.
pub(super) fn stops_required() -> Value {
    serde_json::json!([
        { "required": ["stops"] },
        { "required": ["kind"], "properties": { "kind": { "const": "scalar" } } },
    ])
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
