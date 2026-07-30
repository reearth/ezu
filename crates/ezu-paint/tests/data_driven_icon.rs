//! Data-driven `icon-image`: a `stamp` with a `name-expr` (the MapLibre
//! `icon-image` expression) picks each feature's icon by name and crops it
//! from a bound sprite sheet, instead of stamping one fixed `image`.

mod common;
use common::render_with_features_and_sprite;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::{RasterBuf, SpriteRect, SpriteSheet, TileId};
use std::collections::HashMap;

/// A single point feature at extent coords `(x, y)` with `kind = k`.
fn point_feature(k: &str, x: i32, y: i32) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("kind".to_string(), Value::String(k.to_string()));
    let mut geometry = Geometry::default();
    geometry.points.push((x, y));
    Feature {
        id: None,
        geometry,
        properties,
    }
}

/// An atlas with two 8×8 icons side by side: `red` at x=0, `blue` at x=8.
fn two_icon_sheet() -> SpriteSheet {
    let mut atlas = RasterBuf::new(16, 8);
    for y in 0..8 {
        for x in 0..16 {
            let i = ((y * 16 + x) * 4) as usize;
            let color = if x < 8 {
                [255u8, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            };
            atlas.pixels[i..i + 4].copy_from_slice(&color);
        }
    }
    let mut icons = HashMap::new();
    icons.insert(
        "red".to_string(),
        SpriteRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
            pixel_ratio: 1.0,
            ..SpriteRect::default()
        },
    );
    icons.insert(
        "blue".to_string(),
        SpriteRect {
            x: 8,
            y: 0,
            width: 8,
            height: 8,
            pixel_ratio: 1.0,
            ..SpriteRect::default()
        },
    );
    SpriteSheet { atlas, icons }
}

#[test]
fn name_expr_crops_per_feature_icon() {
    // Two points: `kind=red` (left) and `kind=blue` (right). The stamp's
    // `name-expr = ["get","kind"]` names each feature's icon, cropped from the
    // bound sheet, so the left half of the canvas ends up red and the right
    // half blue.
    let layer = FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features: vec![
            point_feature("red", 1024, 2048),
            point_feature("blue", 3072, 2048),
        ],
    };

    let json = r##"{
      "name": "dd-icon",
      "tile-size": 64,
      "sources": {
        "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" },
        "sheet": { "type": "sprite", "image": "builtin:atlas",
                   "index": { "red":  {"x":0,"y":0,"width":8,"height":8},
                              "blue": {"x":8,"y":0,"width":8,"height":8} } }
      },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "pts" },
        "out":   { "op": "stamp", "features": "@feats", "sprite": "@sheet",
                   "name-expr": ["get","kind"] }
      },
      "output": "@out"
    }"##;

    let r = render_with_features_and_sprite(
        json,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer)],
        "atlas",
        two_icon_sheet(),
    );

    // Count red-dominant / blue-dominant opaque pixels in each half.
    let count = |x_lo: u32, x_hi: u32, red: bool| -> usize {
        let mut n = 0;
        for y in 0..r.height {
            for x in x_lo..x_hi {
                let p = r.pixel(x, y);
                if p[3] > 100 && ((red && p[0] > p[2]) || (!red && p[2] > p[0])) {
                    n += 1;
                }
            }
        }
        n
    };
    let red_left = count(0, 32, true);
    let blue_left = count(0, 32, false);
    let red_right = count(32, 64, true);
    let blue_right = count(32, 64, false);

    assert!(
        red_left > 0 && red_left > blue_left,
        "left half should be red (red={red_left}, blue={blue_left})"
    );
    assert!(
        blue_right > 0 && blue_right > red_right,
        "right half should be blue (red={red_right}, blue={blue_right})"
    );
}

#[test]
fn unknown_icon_name_is_skipped() {
    // A feature whose `name-expr` resolves to an icon the sheet lacks stamps
    // nothing (rather than failing the tile), while a known-name feature in
    // the same layer still paints.
    let layer = FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features: vec![
            point_feature("missing", 1024, 2048),
            point_feature("red", 3072, 2048),
        ],
    };

    let json = r##"{
      "name": "dd-icon-missing",
      "tile-size": 64,
      "sources": {
        "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" },
        "sheet": { "type": "sprite", "image": "builtin:atlas",
                   "index": { "red": {"x":0,"y":0,"width":8,"height":8} } }
      },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "pts" },
        "out":   { "op": "stamp", "features": "@feats", "sprite": "@sheet",
                   "name-expr": ["get","kind"] }
      },
      "output": "@out"
    }"##;

    let r = render_with_features_and_sprite(
        json,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer)],
        "atlas",
        two_icon_sheet(),
    );

    let opaque_in = |x_lo: u32, x_hi: u32| -> usize {
        let mut n = 0;
        for y in 0..r.height {
            for x in x_lo..x_hi {
                if r.pixel(x, y)[3] > 100 {
                    n += 1;
                }
            }
        }
        n
    };
    assert_eq!(
        opaque_in(0, 32),
        0,
        "unknown-icon feature should paint nothing"
    );
    assert!(
        opaque_in(32, 64) > 0,
        "known-icon feature should still paint"
    );
}
