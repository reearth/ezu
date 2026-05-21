//! Shared helpers for `gradient-*` nodes.

use ezu_graph::EvalCtx;

use crate::nodes::common::Anchor;

/// Map a padded-canvas pixel to a "user-space" `(ux, uy)` coordinate
/// that the gradient's parameters are expressed in.
///
/// - `Tile` anchor: tile spans `[0, 1] × [0, 1]`. Padding pixels
///   extend slightly outside that range, which is intentional — the
///   gradient continues into the padded border consistently.
/// - `World` anchor: the full Mercator world at z=0 spans `[0, 1] ×
///   [0, 1]`, with `(0, 0)` at the top-left. The current tile occupies
///   `[x/2^z, (x+1)/2^z] × [y/2^z, (y+1)/2^z]`. World-anchored gradients
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
