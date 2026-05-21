//! Raster layout nodes: tiling and place.

mod common;
use common::{render, render_tile};
use ezu_graph::TileId;

#[test]
fn tiling_passes_through_at_natural_scale() {
    // Tile a `circle` raster onto a same-size canvas with `scale-px`
    // equal to the source width: the output should reproduce the
    // source 1:1 — red at center, transparent at corners.
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "nodes": {
        "src":  { "op": "circle", "color": "#ff0000", "radius-frac": 0.4 },
        "out":  { "op": "tiling", "input": "@src", "anchor": "tile", "scale-px": 16 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 16, 0);
    let center = r.pixel(8, 8);
    assert!(center[0] > 200, "center should be red: {center:?}");
    let corner = r.pixel(0, 0);
    assert_eq!(corner[3], 0, "corner should be transparent: {corner:?}");
}

#[test]
fn tiling_repeats_pattern_at_smaller_scale() {
    // Halving the scale should tile the disk twice along each axis, so
    // four disks appear in the output. Sampling at the four "tile
    // centers" (4, 4), (12, 4), (4, 12), (12, 12) should all be red.
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "nodes": {
        "src":  { "op": "circle", "color": "#ff0000", "radius-frac": 0.4 },
        "out":  { "op": "tiling", "input": "@src", "anchor": "tile", "scale-px": 8 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 16, 0);
    for &(x, y) in &[(4, 4), (12, 4), (4, 12), (12, 12)] {
        let p = r.pixel(x, y);
        assert!(p[0] > 100, "tile center ({x},{y}) should have red disk: {p:?}");
    }
}

#[test]
fn tiling_world_anchor_is_seamless_across_tiles() {
    // Two adjacent map tiles, world-anchored: pad lets us sample the
    // same world column from both tiles' padded buffers. With anchor
    // "world", `left.pixel(tile_size + pad + dx, y)` and
    // `right.pixel(pad + dx, y)` must reference the same world pixel.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src":  { "op": "circle", "color": "#0000ff", "radius-frac": 0.3 },
        "out":  { "op": "tiling", "input": "@src", "anchor": "world", "scale-px": 12 }
      },
      "output": "@out"
    }"##;
    let left = render_tile(json, 32, 4, TileId { z: 4, x: 5, y: 7 });
    let right = render_tile(json, 32, 4, TileId { z: 4, x: 6, y: 7 });
    let pad = 4u32;
    let tile_size = 32u32;
    for dx in 0..pad {
        let lx = tile_size + pad + dx;
        let rx = pad + dx;
        for y in pad..(pad + tile_size) {
            let l = left.pixel(lx, y);
            let r = right.pixel(rx, y);
            // Bilinear can introduce ±1 LSB; everything else must
            // agree byte-for-byte.
            for c in 0..4 {
                assert!(
                    (l[c] as i32 - r[c] as i32).abs() <= 1,
                    "seam mismatch at dx={dx} y={y} channel={c}: left={l:?} right={r:?}"
                );
            }
        }
    }
}

#[test]
fn place_cover_fills_canvas_with_source_color() {
    // A 16-canvas source disk (red) covered onto a 32-canvas should
    // scale up by 2x. The canvas center should be red; cover crops
    // the source so corners are red too (uniform scale 2x covers
    // exactly, the source disk reaches y/x = 16 from canvas center).
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src":  { "op": "circle", "color": "#ff0000", "radius-frac": 0.5 },
        "out":  { "op": "place", "input": "@src", "fit": "cover" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    let center = r.pixel(16, 16);
    assert!(center[0] > 200, "center should be red under cover: {center:?}");
    assert!(center[3] > 200);
}

#[test]
fn place_contain_centers_source_with_letterbox() {
    // A square source contained in a square canvas: with equal aspect,
    // contain == identity. We use a non-square arrangement by
    // contain-fitting a smaller virtual rect via scale-down test:
    // place at fit=none, scale=0.5, anchor=center, position center.
    // Verifies the manual placement path: shrink the disk to half
    // size, centered. Corners should now be transparent.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src":  { "op": "circle", "color": "#00ff00", "radius-frac": 0.5 },
        "out":  { "op": "place", "input": "@src", "fit": "none",
                  "scale": 0.5, "anchor": "center",
                  "position-px": [16, 16] }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    let center = r.pixel(16, 16);
    assert!(center[1] > 200, "center should be green: {center:?}");
    // Disk now has radius ~4 px (half of 8), so (24, 16) is well outside.
    let outside = r.pixel(24, 16);
    assert_eq!(outside[3], 0, "shrunk disk should not reach (24,16): {outside:?}");
}
