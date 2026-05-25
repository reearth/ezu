//! Raster layout nodes: tiling and place. Both consume `Sprite` (raw
//! image dimensions, native to the asset) and produce `Raster`
//! (canvas-padded). These tests use an in-memory image bank so they
//! don't touch the filesystem.

mod common;
use common::{disk_sprite, render_with_images, solid_sprite};
use ezu_graph::{build_graph, BuildError, PortKind, TileId};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

#[test]
fn image_directly_as_output_is_rejected() {
    // Wiring `image` straight into `output` would feed a raw sprite
    // to the host's `raster_to_png` crop and silently mis-align. The
    // graph builder catches this at build time via OutputKindMismatch.
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "assets": { "icon": { "type": "image", "src": "icon" } },
      "nodes": { "src": { "op": "image", "src": "@icon" } },
      "output": "@src"
    }"##;
    let doc = Document::from_json(json).expect("parse");
    let registry = default_registry();
    match build_graph(&doc, &registry) {
        Err(ezu_graph::BuildGraphError::Graph(BuildError::OutputKindMismatch { node, got })) => {
            assert_eq!(node, "src");
            assert_eq!(got, PortKind::Sprite);
        }
        other => panic!("expected OutputKindMismatch, got {other:?}"),
    }
}

#[test]
fn tiling_passes_through_at_natural_scale() {
    // Source sprite is a 16×16 disk; tiling at `scale-px: 16` on a
    // 16-canvas reproduces it 1:1 — red at center, transparent at
    // corners.
    let sprite = disk_sprite(16, 16, 6.0, [255, 0, 0, 255]);
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "assets": { "disk": { "type": "image", "src": "disk" } },
      "nodes": {
        "src":  { "op": "image", "src": "@disk" },
        "out":  { "op": "tiling", "input": "@src", "anchor": "tile", "scale-px": 16 }
      },
      "output": "@out"
    }"##;
    let r = render_with_images(
        json,
        16,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("disk", sprite)],
    );
    let center = r.pixel(8, 8);
    assert!(center[0] > 200, "center should be red: {center:?}");
    let corner = r.pixel(0, 0);
    assert_eq!(corner[3], 0, "corner should be transparent: {corner:?}");
}

#[test]
fn tiling_repeats_pattern_at_smaller_scale() {
    // Halving the scale should tile a 16×16 disk twice along each
    // axis. Sampling at the four tile centers — (4,4), (12,4),
    // (4,12), (12,12) — should all land inside a copy of the disk.
    let sprite = disk_sprite(16, 16, 6.0, [255, 0, 0, 255]);
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "assets": { "disk": { "type": "image", "src": "disk" } },
      "nodes": {
        "src":  { "op": "image", "src": "@disk" },
        "out":  { "op": "tiling", "input": "@src", "anchor": "tile", "scale-px": 8 }
      },
      "output": "@out"
    }"##;
    let r = render_with_images(
        json,
        16,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("disk", sprite)],
    );
    for &(x, y) in &[(4, 4), (12, 4), (4, 12), (12, 12)] {
        let p = r.pixel(x, y);
        assert!(
            p[0] > 100,
            "tile center ({x},{y}) should have red disk: {p:?}"
        );
    }
}

#[test]
fn tiling_world_anchor_is_seamless_across_tiles() {
    // Two adjacent map tiles, world-anchored: with `anchor: world`,
    // sampling the same world column from both tiles' padded buffers
    // must produce identical pixels (±1 LSB for bilinear rounding).
    let sprite = solid_sprite(8, 8, [0, 0, 255, 255]);
    // Note: a solid sprite repeats trivially; use a small spatial
    // gradient by combining a disk so the seam test isn't degenerate.
    let sprite = {
        let mut s = sprite;
        // Punch a transparent hole in one corner so wrapping creates
        // a visible pattern variation per cell.
        for y in 0..3 {
            for x in 0..3 {
                let i = ((y * 8 + x) * 4) as usize;
                s.pixels[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
            }
        }
        s
    };
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "assets": { "p": { "type": "image", "src": "p" } },
      "nodes": {
        "src":  { "op": "image", "src": "@p" },
        "out":  { "op": "tiling", "input": "@src", "anchor": "world", "scale-px": 12 }
      },
      "output": "@out"
    }"##;
    let left = render_with_images(
        json,
        32,
        4,
        TileId { z: 4, x: 5, y: 7 },
        &[("p", sprite.clone())],
    );
    let right = render_with_images(json, 32, 4, TileId { z: 4, x: 6, y: 7 }, &[("p", sprite)]);
    let pad = 4u32;
    let tile_size = 32u32;
    for dx in 0..pad {
        let lx = tile_size + pad + dx;
        let rx = pad + dx;
        for y in pad..(pad + tile_size) {
            let l = left.pixel(lx, y);
            let r = right.pixel(rx, y);
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
    // 16×16 sprite (red disk) covered onto a 32-canvas: uniform scale
    // 2× covers exactly. The canvas center should be red.
    let sprite = disk_sprite(16, 16, 8.0, [255, 0, 0, 255]);
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "assets": { "src": { "type": "image", "src": "src" } },
      "nodes": {
        "src":  { "op": "image", "src": "@src" },
        "out":  { "op": "place", "input": "@src", "fit": "cover" }
      },
      "output": "@out"
    }"##;
    let r = render_with_images(json, 32, 0, TileId { z: 0, x: 0, y: 0 }, &[("src", sprite)]);
    let center = r.pixel(16, 16);
    assert!(
        center[0] > 200,
        "center should be red under cover: {center:?}"
    );
    assert!(center[3] > 200);
}

#[test]
fn place_none_with_scale_and_anchor_shrinks_source() {
    // fit=none + scale=0.5 + anchor=center + position=(16,16):
    // a 16×16 disk sprite is rendered at half size, centered on a
    // 32-canvas. The disk shrinks to ~8 px wide; (24, 16) is outside.
    let sprite = disk_sprite(16, 16, 8.0, [0, 255, 0, 255]);
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "assets": { "src": { "type": "image", "src": "src" } },
      "nodes": {
        "src":  { "op": "image", "src": "@src" },
        "out":  { "op": "place", "input": "@src", "fit": "none",
                  "scale": 0.5, "anchor": "center",
                  "position-px": [16, 16] }
      },
      "output": "@out"
    }"##;
    let r = render_with_images(json, 32, 0, TileId { z: 0, x: 0, y: 0 }, &[("src", sprite)]);
    let center = r.pixel(16, 16);
    assert!(center[1] > 200, "center should be green: {center:?}");
    let outside = r.pixel(24, 16);
    assert_eq!(
        outside[3], 0,
        "shrunk disk should not reach (24,16): {outside:?}"
    );
}

#[test]
fn hsl_passes_through_sprite_kind() {
    // `hsl` is polymorphic; a red disk sprite rotated 120° (red → green)
    // should still type-check through `place` and render green at the
    // canvas center.
    let sprite = disk_sprite(16, 16, 6.0, [255, 0, 0, 255]);
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "assets": { "src": { "type": "image", "src": "src" } },
      "nodes": {
        "src":     { "op": "image", "src": "@src" },
        "shifted": { "op": "hsl",   "input": "@src", "hue-shift": 120.0 },
        "out":     { "op": "place", "input": "@shifted", "fit": "cover" }
      },
      "output": "@out"
    }"##;
    let r = render_with_images(json, 32, 0, TileId { z: 0, x: 0, y: 0 }, &[("src", sprite)]);
    let center = r.pixel(16, 16);
    assert!(
        center[1] > 150 && center[0] < 80,
        "hue-shifted disk center should be greenish: {center:?}"
    );
}

#[test]
fn blur_passes_through_sprite_kind() {
    // `blur` is polymorphic over Raster/Sprite. Feeding a Sprite
    // (`image`) through `blur` into `place` (Sprite-only) must
    // type-check at graph build time, and the blurred sprite must
    // render through place correctly.
    let sprite = disk_sprite(16, 16, 6.0, [255, 0, 0, 255]);
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "assets": { "src": { "type": "image", "src": "src" } },
      "nodes": {
        "src":     { "op": "image", "src": "@src" },
        "blurred": { "op": "blur",  "input": "@src", "sigma": 1.5 },
        "out":     { "op": "place", "input": "@blurred", "fit": "cover" }
      },
      "output": "@out"
    }"##;
    let r = render_with_images(json, 32, 0, TileId { z: 0, x: 0, y: 0 }, &[("src", sprite)]);
    let center = r.pixel(16, 16);
    assert!(center[0] > 150, "blurred disk center should still be reddish: {center:?}");
}
