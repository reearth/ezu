//! A sprite icon draws exactly its own pixels: nothing from the atlas
//! neighbour beside it, and no copy of its own edge past its rect.
//!
//! Every draw here centres an odd-sized icon on a whole pixel, which puts
//! the icon's edges on half pixels — the placement where the drawn rect
//! rounds one pixel past the image. The icon marks its first column green
//! and its last column yellow, so a one-pixel slip shows up as a missing
//! green column and a doubled yellow one; its atlas neighbour is solid red.

use crate::common::render_with_features_and_sprite;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::{RasterBuf, SpriteRect, SpriteSheet, TileId};
use std::collections::HashMap;

const TILE: u32 = 64;
const PAD: u32 = 8;
const EXTENT: i32 = 4096;
/// The icon's side, odd so that centring it on a whole pixel puts its edges
/// on half pixels.
const SIDE: u32 = 7;

const GREEN: [u8; 4] = [0, 255, 0, 255];
const YELLOW: [u8; 4] = [255, 255, 0, 255];
const RED: [u8; 4] = [255, 0, 0, 255];

/// `edges` at x=0: green first column, yellow last column, transparent
/// between. `red` right next to it at x=SIDE, solid.
fn sheet() -> SpriteSheet {
    let w = SIDE * 2;
    let mut atlas = RasterBuf::new(w, SIDE);
    for y in 0..SIDE {
        for x in 0..w {
            let c = match x {
                0 => GREEN,
                x if x == SIDE - 1 => YELLOW,
                x if x >= SIDE => RED,
                _ => continue,
            };
            let i = ((y * w + x) * 4) as usize;
            atlas.pixels[i..i + 4].copy_from_slice(&c);
        }
    }
    let rect = |x| SpriteRect {
        x,
        y: 0,
        width: SIDE,
        height: SIDE,
        pixel_ratio: 1.0,
        ..SpriteRect::default()
    };
    let icons = HashMap::from([
        ("edges".to_string(), rect(0)),
        ("red".to_string(), rect(SIDE)),
    ]);
    SpriteSheet { atlas, icons }
}

const SPRITE_SOURCE: &str = r#""sheet": { "type": "sprite", "image": "builtin:atlas",
    "index": { "edges": { "x": 0, "y": 0, "width": 7, "height": 7 },
               "red":   { "x": 7, "y": 0, "width": 7, "height": 7 } } }"#;

/// A point layer `pts` with features at the given tile-pixel positions,
/// each naming the `edges` icon in `icon`.
fn points(at: &[(u32, u32)]) -> FeatureLayer {
    let u = EXTENT / TILE as i32;
    let features = at
        .iter()
        .map(|&(px, py)| {
            let mut geometry = Geometry::default();
            geometry.points.push((px as i32 * u, py as i32 * u));
            Feature {
                id: None,
                geometry,
                properties: HashMap::from([
                    ("icon".to_string(), Value::String("edges".to_string())),
                    ("name".to_string(), Value::String(" ".to_string())),
                ]),
            }
        })
        .collect();
    FeatureLayer {
        name: "pts".to_string(),
        extent: EXTENT as u32,
        features,
    }
}

fn render(nodes: &str, pts: FeatureLayer) -> std::sync::Arc<RasterBuf> {
    let font = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    let font = format!("file:{}", font.display()).replace('\\', "/");
    let recipe = format!(
        r#"{{
      "name": "sprite-icon-edges",
      "tile-size": {TILE},
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }},
        {SPRITE_SOURCE}
      }},
      "nodes": {{
        "pts": {{ "op": "features", "source": "src", "layer": "pts" }},
        {nodes}
      }},
      "output": "@out"
    }}"#
    );
    render_with_features_and_sprite(
        &recipe,
        TILE,
        PAD,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", pts)],
        "atlas",
        sheet(),
    )
}

/// Each drawn icon must show one green column and one yellow column,
/// `SIDE - 1` pixels apart, and nothing else: no red, no second yellow.
fn assert_exact_icons(r: &RasterBuf, icons: usize) {
    let (mut green, mut yellow) = (Vec::new(), Vec::new());
    for y in 0..r.height {
        for x in 0..r.width {
            let p = r.pixel(x, y);
            if p[3] == 0 {
                continue;
            }
            assert!(
                p[3] == 255 && p[1] == 255 && p[2] == 0,
                "({x},{y}) is {p:?}, which is neither of the icon's colours"
            );
            if p[0] == 0 {
                green.push((x, y));
            } else {
                yellow.push((x, y));
            }
        }
    }
    let n = icons * SIDE as usize;
    assert_eq!(green.len(), n, "green pixels (the first column): {green:?}");
    assert_eq!(
        yellow.len(),
        n,
        "yellow pixels (the last column): {yellow:?}"
    );
    for &(x, y) in &green {
        assert!(
            yellow.contains(&(x + SIDE - 1, y)),
            "green ({x},{y}) has no yellow {} px to its right",
            SIDE - 1
        );
    }
}

#[test]
fn text_icon_draws_only_its_own_pixels() {
    let r = render(
        r#""out": { "op": "text", "features": "@pts", "source": "src", "layer": "pts",
                    "font": ["body"], "text": ["get", "name"], "size": 12,
                    "icon-sprite": "@sheet", "icon-name": "edges",
                    "allow-overlap": true, "icon-allow-overlap": true }"#,
        points(&[(20, 20), (44, 40)]),
    );
    assert_exact_icons(&r, 2);
}

#[test]
fn data_driven_stamp_draws_only_its_own_pixels() {
    let r = render(
        r#""out": { "op": "stamp", "features": "@pts", "sprite": "@sheet",
                    "name-expr": ["get", "icon"] }"#,
        points(&[(20, 20), (44, 40)]),
    );
    assert_exact_icons(&r, 2);
}

#[test]
fn cropped_icon_stamp_draws_only_its_own_pixels() {
    let r = render(
        r#""img": { "op": "icon", "sprite": "@sheet", "name": "edges" },
           "out": { "op": "stamp", "features": "@pts", "image": "@img" }"#,
        points(&[(20, 20), (44, 40)]),
    );
    assert_exact_icons(&r, 2);
}

#[test]
fn line_stamp_draws_only_its_own_pixels() {
    // Spacing 8 puts the first centre 4 px in and every later one 8 px on,
    // all on whole pixels.
    let r = render(
        r#""img":  { "op": "icon", "sprite": "@sheet", "name": "edges" },
           "line": { "op": "literal-geometry", "extent": 4096,
                     "lines": [ [ [1024, 2048], [3072, 2048] ] ] },
           "out":  { "op": "line-stamp", "features": "@line", "image": "@img",
                     "spacing-px": 8 }"#,
        points(&[]),
    );
    // A 32 px line holds stamps at 4, 12, 20 and 28 px along it.
    assert_exact_icons(&r, 4);
}

#[test]
fn placed_icon_draws_only_its_own_pixels() {
    let r = render(
        r#""img": { "op": "icon", "sprite": "@sheet", "name": "edges" },
           "out": { "op": "place", "input": "@img", "position-px": [30, 30],
                    "anchor": "center" }"#,
        points(&[]),
    );
    assert_exact_icons(&r, 1);
}
