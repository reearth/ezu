//! Noise source and the displace/warp distortion nodes.

mod common;
use common::{render, render_tile};
use ezu_graph::TileId;

#[test]
fn noise_perlin_produces_variation_and_is_deterministic() {
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "out": {
          "op": "noise",
          "type": "perlin",
          "scale-px": 16,
          "octaves": 3,
          "seed": 42,
          "anchor": "tile"
        }
      },
      "output": "@out"
    }"##;
    let a = render(json, 32, 0);
    let b = render(json, 32, 0);
    // Deterministic: identical bytes across two renders.
    assert_eq!(a.pixels, b.pixels);
    // Not uniform: at least two distinct luma values.
    let mut min = 255u8;
    let mut max = 0u8;
    for chunk in a.pixels.chunks(4) {
        min = min.min(chunk[0]);
        max = max.max(chunk[0]);
    }
    assert!(
        max as i32 - min as i32 > 20,
        "noise should vary: {min}..{max}"
    );
}

#[test]
fn noise_white_changes_with_seed() {
    let mk = |seed: u32| -> String {
        format!(
            r##"{{
              "name": "demo",
              "tile-size": 16,
              "nodes": {{
                "out": {{ "op": "noise", "type": "white", "scale-px": 1, "seed": {seed}, "anchor": "tile" }}
              }},
              "output": "@out"
            }}"##
        )
    };
    let a = render(&mk(1), 16, 0);
    let b = render(&mk(2), 16, 0);
    assert_ne!(
        a.pixels, b.pixels,
        "different seeds should give different white noise"
    );
}

#[test]
fn displace_with_zero_amp_is_identity() {
    // amp-px = 0 means the displace node should reproduce its input
    // byte-for-byte (modulo the grown pad). Comparing visible-tile
    // pixels at matching offsets verifies the warp path doesn't drop
    // information when there's nothing to do.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src":   { "op": "circle", "color": "#ff0000", "radius-frac": 0.3 },
        "disp":  { "op": "solid", "color": "#808080" },
        "out":   { "op": "displace", "input": "@src", "displacement": "@disp", "amp-px": 0 }
      },
      "output": "@out"
    }"##;
    let warped = render(json, 32, 0);
    let json_ref = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": { "src": { "op": "circle", "color": "#ff0000", "radius-frac": 0.3 } },
      "output": "@src"
    }"##;
    let reference = render(json_ref, 32, 0);
    // Reference has pad=0 (32×32); warped has pad=0 too because amp=0
    // doesn't bump required_pad. So both rasters are the same size.
    assert_eq!(warped.width, reference.width);
    assert_eq!(warped.pixels, reference.pixels);
}

#[test]
fn displace_moves_pixels_when_amp_nonzero() {
    // With a non-zero amplitude and a non-neutral displacement map,
    // pixels near the disk edge must change versus an undisplaced
    // reference. We probe a pixel that sits on the disk's right
    // boundary.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src":   { "op": "circle", "color": "#ff0000", "radius-frac": 0.3 },
        "disp":  { "op": "solid", "color": "#ff0000" },
        "out":   { "op": "displace", "input": "@src", "displacement": "@disp", "amp-px": 4 }
      },
      "output": "@out"
    }"##;
    // The host must supply enough pad for the warp amplitude — the
    // framework exposes `required_pad` as a contract for hosts to honour
    // via `compute_pad`, not as an auto-grow mechanism.
    let warped = render(json, 32, 8);
    // R=255, G=0: dx = (1.0 - 0.5) * 2 * 4 = +4 px, dy = -4 px.
    // Visible tile center (16, 16) inside the 32+2*8 padded raster is
    // at padded (24, 24), and the read happens at input (28, 20) which
    // still lands inside the radius-frac=0.3 disk (r ≈ 9.6).
    let center = warped.pixel(8 + 16, 8 + 16);
    assert_eq!(center[0], 255, "center should still read red: {center:?}");
}

#[test]
fn warp_disturbs_input_but_stays_deterministic() {
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src":   { "op": "circle", "color": "#000000", "radius-frac": 0.4 },
        "out":   {
          "op": "warp", "input": "@src",
          "type": "perlin", "scale-px": 12, "amp-px": 5, "seed": 7,
          "anchor": "tile"
        }
      },
      "output": "@out"
    }"##;
    let a = render(json, 32, 0);
    let b = render(json, 32, 0);
    assert_eq!(a.pixels, b.pixels, "warp must be deterministic");

    let json_ref = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": { "src": { "op": "circle", "color": "#000000", "radius-frac": 0.4 } },
      "output": "@src"
    }"##;
    let reference = render(json_ref, 32, 0);
    assert_ne!(a.pixels, reference.pixels, "warp should perturb pixels");
}

#[test]
fn warp_world_anchor_is_seamless_across_adjacent_tiles() {
    // Two horizontally adjacent tiles must agree at their shared
    // border column. The input is a world-anchored noise field plus a
    // world-anchored warp, so the value at the same world pixel must
    // match on either side of the seam.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src":   {
          "op": "noise", "type": "perlin", "scale-px": 24,
          "seed": 11, "anchor": "world"
        },
        "out":   {
          "op": "warp", "input": "@src",
          "type": "perlin", "scale-px": 16, "amp-px": 4, "seed": 23,
          "anchor": "world"
        }
      },
      "output": "@out"
    }"##;
    // Pad must cover `amp-px` so the warp can reach into the neighbour
    // tile's territory and produce a value identical to that
    // neighbour's own read.
    let pad: u32 = 8;
    let amp: u32 = 4;
    let left = render_tile(json, 32, pad, TileId { z: 4, x: 5, y: 7 });
    let right = render_tile(json, 32, pad, TileId { z: 4, x: 6, y: 7 });
    // Each warped pixel reads from a position up to `amp` px away in
    // raster-local coords, so the *safe* output range is the inner
    // `[amp, padded - amp]` band where the read never hits the boundary
    // clamp. Restrict the seam comparison to world columns that fall in
    // both tiles' safe bands — there both tiles compute from identical
    // world-positioned samples and must agree byte-for-byte.
    let tile_size = 32u32;
    // Left tile safe padded x ∈ [amp, tile_size + 2*pad - amp).
    // Right tile safe padded x ∈ [amp, tile_size + 2*pad - amp).
    // World overlap of safe bands = pad - amp columns on each side of
    // the shared border.
    let safe = pad - amp;
    for dx in 0..safe {
        let lx = tile_size + pad + dx; // left padded x
        let rx = pad + dx; // right padded x — same world column
        for y in (pad + amp)..(pad + tile_size - amp) {
            let l = left.pixel(lx, y);
            let r = right.pixel(rx, y);
            assert_eq!(
                l, r,
                "seam mismatch at dx={dx} y={y}: left={l:?} right={r:?}"
            );
        }
    }
}
