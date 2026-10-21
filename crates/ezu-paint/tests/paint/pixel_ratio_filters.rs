//! Raster filters on a denser canvas. A tile at pixel ratio 2 shows the
//! same map as the ratio-1 tile at twice the density, so every px field
//! of a filter covers twice as many canvas pixels, and a world-anchored
//! field samples the same world positions. A sprite's pixels stay style
//! pixels at every ratio.

use std::sync::Arc;

use ezu_graph::{
    build_graph, Cache, CanvasInfo, Evaluator, ParamValues, PortValue, RasterBuf, TileId,
};
use ezu_paint::host::BrushBankLoader;
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

use crate::common::solid_sprite;

const Z0: TileId = TileId { z: 0, x: 0, y: 0 };

/// Render `json` on a `tile_size`-style-pixel tile at `ratio` device
/// pixels per style pixel, with no margin.
fn render_at(
    json: &str,
    tile_size: u32,
    ratio: u32,
    tile: TileId,
    images: &[(&str, RasterBuf)],
) -> Arc<RasterBuf> {
    let doc = Document::from_json(json).expect("parse");
    let registry = default_registry();
    let graph = build_graph(&doc, &registry).expect("build");
    let cache = Cache::new();
    let mut loader = BrushBankLoader::new();
    for (name, img) in images {
        loader.insert_image(name.to_string(), img.clone());
    }
    let canvas = CanvasInfo::square(tile_size * ratio, 0).with_pixel_ratio(ratio as f32);
    let out = Evaluator::new(&graph, &cache, &loader)
        .render(tile, canvas, &ParamValues::new(), 0)
        .expect("render");
    match out {
        PortValue::Raster(r) => r,
        other => panic!("expected raster output, got {:?}", other.kind()),
    }
}

/// Average each 2×2 block, taking a ratio-2 render back to ratio-1 size.
fn downsample2(r: &RasterBuf) -> RasterBuf {
    let (w, h) = (r.width / 2, r.height / 2);
    let mut out = RasterBuf::new(w, h);
    for y in 0..h {
        for x in 0..w {
            for c in 0..4 {
                let s: u32 = [(0, 0), (1, 0), (0, 1), (1, 1)]
                    .iter()
                    .map(|&(dx, dy)| r.pixel(2 * x + dx, 2 * y + dy)[c] as u32)
                    .sum();
                out.pixels[((y * w + x) * 4 + c as u32) as usize] = ((s + 2) / 4) as u8;
            }
        }
    }
    out
}

fn max_channel_diff(a: &RasterBuf, b: &RasterBuf) -> u8 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.pixels
        .iter()
        .zip(&b.pixels)
        .map(|(p, q)| p.abs_diff(*q))
        .max()
        .unwrap_or(0)
}

/// Every pixel of the ratio-1 render against the ratio-2 pixel at the
/// same style position, `(2x, 2y)`.
fn max_diff_at_matching_points(one: &RasterBuf, two: &RasterBuf) -> u8 {
    let mut worst = 0;
    for y in 0..one.height {
        for x in 0..one.width {
            let (p, q) = (one.pixel(x, y), two.pixel(2 * x, 2 * y));
            for c in 0..4 {
                worst = worst.max(p[c].abs_diff(q[c]));
            }
        }
    }
    worst
}

#[test]
fn blur_spreads_over_the_same_ground_at_double_density() {
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "disk": { "op": "circle", "color": "#ffffff", "radius-frac": 0.3 },
        "out":  { "op": "blur", "input": "@disk", "sigma": 3 }
      },
      "output": "@out"
    }"##;
    let one = render_at(json, 32, 1, Z0, &[]);
    let two = render_at(json, 32, 2, Z0, &[]);
    let diff = max_channel_diff(&one, &downsample2(&two));
    assert!(diff <= 6, "ratio-2 blur, downsampled, is off by {diff}");
}

#[test]
fn world_noise_samples_the_same_field_at_double_density() {
    // `warp-amp` is in pixels too, so the domain warp lines up as well.
    let json = r##"{
      "name": "demo",
      "tile-size": 24,
      "nodes": {
        "out": { "op": "noise", "type": "perlin", "scale-px": 7.3, "octaves": 3,
                 "warp-amp": 2.5, "anchor": "world" }
      },
      "output": "@out"
    }"##;
    let tile = TileId { z: 4, x: 3, y: 9 };
    let one = render_at(json, 24, 1, tile, &[]);
    let two = render_at(json, 24, 2, tile, &[]);
    assert_eq!(max_diff_at_matching_points(&one, &two), 0);
}

#[test]
fn warp_displaces_by_the_same_ground_at_double_density() {
    // A horizontal ramp of 8 levels per style pixel: a displacement off
    // by a pixel would show as a difference of about 8.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "ramp": { "op": "gradient-linear", "start": [0, 0.5], "end": [1, 0.5],
                  "stops": [[0, "#000000"], [1, "#ffffff"]] },
        "out":  { "op": "warp", "input": "@ramp", "scale-px": 9, "amp-px": 6,
                  "anchor": "world" }
      },
      "output": "@out"
    }"##;
    let tile = TileId { z: 3, x: 5, y: 2 };
    let one = render_at(json, 32, 1, tile, &[]);
    let two = render_at(json, 32, 2, tile, &[]);
    // Sampled away from the clamped tile edges.
    let mut worst = 0;
    for y in 0..32 {
        for x in 8..24 {
            worst = worst.max(one.pixel(x, y)[0].abs_diff(two.pixel(2 * x, 2 * y)[0]));
        }
    }
    assert!(worst <= 4, "ratio-2 warp is off by {worst}");
}

/// Pixels along the middle row that are more than half covered.
fn covered_on_middle_row(r: &RasterBuf) -> u32 {
    let y = r.height / 2;
    (0..r.width).filter(|&x| r.pixel(x, y)[3] > 127).count() as u32
}

#[test]
fn dilate_grows_by_the_same_ground_at_double_density() {
    let json = r##"{
      "name": "demo",
      "tile-size": 48,
      "nodes": {
        "disk": { "op": "circle", "color": "#ffffff", "radius-frac": 0.2 },
        "out":  { "op": "dilate", "input": "@disk", "radius-px": 5 }
      },
      "output": "@out"
    }"##;
    let one = covered_on_middle_row(&render_at(json, 48, 1, Z0, &[]));
    let two = covered_on_middle_row(&render_at(json, 48, 2, Z0, &[]));
    // A 9.6 px disk grown by 5 px each side.
    assert!((28..=31).contains(&one), "ratio 1 covers {one}");
    assert!(
        two.abs_diff(2 * one) <= 2,
        "ratio 2 covers {two}, ratio 1 {one}"
    );
}

/// An 8×8 sprite of vertical stripes: opaque for `x < 4`.
fn stripes() -> RasterBuf {
    let mut s = solid_sprite(8, 8, [255, 0, 0, 255]);
    for y in 0..8 {
        for x in 4..8 {
            let i = ((y * 8 + x) * 4) as usize;
            s.pixels[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
        }
    }
    s
}

#[test]
fn tiling_repeats_a_sprite_over_the_same_ground_at_double_density() {
    for fields in [
        r#""anchor": "tile""#,
        r#""anchor": "world", "scale-px": 12, "offset-px": [3, 5]"#,
    ] {
        let json = format!(
            r##"{{
              "name": "demo",
              "tile-size": 32,
              "sources": {{ "stripes": {{ "type": "image", "src": "builtin:stripes" }} }},
              "nodes": {{
                "src": {{ "op": "image", "src": "@stripes" }},
                "out": {{ "op": "tiling", "input": "@src", {fields} }}
              }},
              "output": "@out"
            }}"##
        );
        let tile = TileId { z: 2, x: 1, y: 3 };
        let images = [("stripes", stripes())];
        let one = render_at(&json, 32, 1, tile, &images);
        let two = render_at(&json, 32, 2, tile, &images);
        assert_eq!(max_diff_at_matching_points(&one, &two), 0, "{fields}");
    }
}

#[test]
fn a_sprite_is_blurred_in_style_pixels_at_any_ratio() {
    // The blurred sprite is tiled at its own size, which puts its pixels
    // at the same style positions at both ratios. Blurring it by twice
    // the sigma at ratio 2 would change them.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "sources": { "stripes": { "type": "image", "src": "builtin:stripes" } },
      "nodes": {
        "src":  { "op": "image", "src": "@stripes" },
        "soft": { "op": "blur", "input": "@src", "sigma": 1.5 },
        "out":  { "op": "tiling", "input": "@soft", "anchor": "tile" }
      },
      "output": "@out"
    }"##;
    let images = [("stripes", stripes())];
    let one = render_at(json, 32, 1, Z0, &images);
    let two = render_at(json, 32, 2, Z0, &images);
    assert_eq!(max_diff_at_matching_points(&one, &two), 0);
}

/// The smallest box holding every more-than-half-covered pixel.
fn covered_box(r: &RasterBuf) -> (u32, u32, u32, u32) {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..r.height {
        for x in 0..r.width {
            if r.pixel(x, y)[3] > 127 {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    (x0, y0, x1, y1)
}

#[test]
fn place_draws_a_sprite_over_the_same_ground_at_double_density() {
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "sources": { "dot": { "type": "image", "src": "builtin:dot" } },
      "nodes": {
        "src": { "op": "image", "src": "@dot" },
        "out": { "op": "place", "input": "@src", "position-px": [5, 7], "scale": 1.5 }
      },
      "output": "@out"
    }"##;
    let images = [("dot", solid_sprite(6, 6, [0, 0, 255, 255]))];
    let one = covered_box(&render_at(json, 32, 1, Z0, &images));
    let two = covered_box(&render_at(json, 32, 2, Z0, &images));
    assert_eq!(one, (5, 7, 14, 16));
    assert_eq!(two, (10, 14, 28, 32));
}

#[test]
fn hillshade_of_a_field_without_ground_scale_keeps_its_relief() {
    // A noise field has no metres per pixel; its gradients are taken
    // per style pixel, so a denser canvas shades it the same.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "n":   { "op": "noise", "kind": "scalar", "type": "perlin", "scale-px": 12 },
        "h":   { "op": "map-range", "field": "@n", "in-min": -1, "in-max": 1,
                 "out-min": 0, "out-max": 6 },
        "out": { "op": "hillshade", "field": "@h" }
      },
      "output": "@out"
    }"##;
    let tile = TileId { z: 5, x: 7, y: 11 };
    let one = render_at(json, 32, 1, tile, &[]);
    let two = render_at(json, 32, 2, tile, &[]);
    let mut worst = 0;
    for y in 1..31 {
        for x in 1..31 {
            worst = worst.max(one.pixel(x, y)[0].abs_diff(two.pixel(2 * x, 2 * y)[0]));
        }
    }
    assert!(worst <= 12, "ratio-2 hillshade is off by {worst}");
}

/// Every pixel of the ratio-1 render against the 2×2 block of ratio-2
/// pixels covering the same style pixel.
fn max_diff_over_blocks(one: &RasterBuf, two: &RasterBuf) -> u8 {
    let mut worst = 0;
    for y in 0..one.height {
        for x in 0..one.width {
            let p = one.pixel(x, y);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let q = two.pixel(2 * x + dx, 2 * y + dy);
                for c in 0..4 {
                    worst = worst.max(p[c].abs_diff(q[c]));
                }
            }
        }
    }
    worst
}

/// Black on the left half of the tile, `right` on the other, meeting on
/// a pixel edge at every ratio.
fn halves(right: &str, filter: &str) -> String {
    format!(
        r##"{{
      "name": "demo",
      "tile-size": 32,
      "nodes": {{
        "step": {{ "op": "gradient-linear", "start": [0, 0.5], "end": [1, 0.5],
                   "stops": [[0.5, "#000000"], [0.5, "{right}"]] }},
        "out":  {filter}
      }},
      "output": "@out"
    }}"##
    )
}

#[test]
fn edge_detect_finds_edges_as_wide_at_double_density() {
    // The kernel's neighbours sit a style pixel away, so the band it draws
    // along a step covers the same style pixels at either ratio, where
    // reading the device pixel either side would draw it half as wide.
    let json = halves(
        "#ffffff",
        r#"{ "op": "edge-detect", "input": "@step", "strength": 0.2 }"#,
    );
    let one = render_at(&json, 32, 1, Z0, &[]);
    let two = render_at(&json, 32, 2, Z0, &[]);
    assert_eq!(one.pixel(15, 8)[0], 204, "the band is drawn");
    assert_eq!(max_diff_over_blocks(&one, &two), 0);
}

#[test]
fn sharpen_rings_as_wide_at_double_density() {
    // A step from black to grey: the overshoot sharpening draws on the
    // grey side of it spans the same style pixels at either ratio.
    let json = halves(
        "#808080",
        r#"{ "op": "sharpen", "input": "@step", "amount": 1 }"#,
    );
    let one = render_at(&json, 32, 1, Z0, &[]);
    let two = render_at(&json, 32, 2, Z0, &[]);
    assert_eq!(one.pixel(16, 8)[0], 255, "the overshoot is drawn");
    assert_eq!(one.pixel(17, 8)[0], 128, "one style pixel wide");
    assert_eq!(max_diff_over_blocks(&one, &two), 0);
}
