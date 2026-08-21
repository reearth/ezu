//! `circles` paint node: constant and data-driven per-feature disks, plus
//! color-conversion parity between a constant `color` and an equivalent
//! opaque `color-expr`.

mod common;
use common::render_with_features;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

/// A single point feature at extent coords `(x, y)` with property `c`.
fn point_feature(c: &str, x: i32, y: i32) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("c".to_string(), Value::String(c.to_string()));
    let mut geometry = Geometry::default();
    geometry.points.push((x, y));
    Feature {
        id: None,
        geometry,
        properties,
    }
}

fn points_layer(name: &str, feats: Vec<Feature>) -> FeatureLayer {
    FeatureLayer {
        name: name.to_string(),
        extent: 4096,
        features: feats,
    }
}

#[test]
fn constant_circle_paints_red_at_point_and_clear_away() {
    // One point at tile center (extent 2048/4096 → px 32 of 64). A constant
    // red circle paints red pixels there and transparent far away.
    let layer = points_layer("pts", vec![point_feature("x", 2048, 2048)]);

    let json = r##"{
      "name": "circle-const",
      "tile-size": 64,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "pts" },
        "out":   { "op": "circles", "features": "@feats", "radius": 8, "color": "#ff0000" }
      },
      "output": "@out"
    }"##;

    let r = render_with_features(
        json,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer)],
    );

    let center = r.pixel(32, 32);
    assert!(
        center[0] > 150 && center[1] < 80 && center[2] < 80 && center[3] > 150,
        "center should be red: {center:?}"
    );
    let corner = r.pixel(2, 2);
    assert!(corner[3] < 20, "corner should be transparent: {corner:?}");
}

#[test]
fn color_expr_paints_two_points_their_classes() {
    // Two points: `c=a` (left) red, `c=b` (right) green, via a match color-expr.
    let layer = points_layer(
        "pts",
        vec![
            point_feature("a", 1024, 2048),
            point_feature("b", 3072, 2048),
        ],
    );

    let json = r##"{
      "name": "circle-dd",
      "tile-size": 64,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "pts" },
        "out":   { "op": "circles", "features": "@feats", "radius": 6, "color": "#000000",
                   "color-expr": ["match", ["get","c"], "a", "#ff0000", "#00ff00"] }
      },
      "output": "@out"
    }"##;

    let r = render_with_features(
        json,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer)],
    );

    // Left point ~px 16, right ~px 48, both at row 32.
    let left = r.pixel(16, 32);
    assert!(
        left[0] > 150 && left[1] < 80 && left[3] > 150,
        "left point should be red: {left:?}"
    );
    let right = r.pixel(48, 32);
    assert!(
        right[1] > 150 && right[0] < 80 && right[3] > 150,
        "right point should be green: {right:?}"
    );
}

#[test]
fn opaque_color_expr_matches_constant_color_pixel_for_pixel() {
    // Parity: an opaque constant `color` must paint the exact same pixels as
    // an equivalent `color-expr` at the center.
    let layer = || points_layer("pts", vec![point_feature("x", 2048, 2048)]);

    let const_json = r##"{
      "name": "const",
      "tile-size": 64,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "pts" },
        "out":   { "op": "circles", "features": "@feats", "radius": 8, "color": "#ff0000" }
      },
      "output": "@out"
    }"##;

    let expr_json = r##"{
      "name": "expr",
      "tile-size": 64,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "pts" },
        "out":   { "op": "circles", "features": "@feats", "radius": 8, "color": "#000000",
                   "color-expr": ["match", ["get","c"], "nope", "#000000", "#ff0000"] }
      },
      "output": "@out"
    }"##;

    let tile = TileId { z: 0, x: 0, y: 0 };
    let a = render_with_features(const_json, 64, 0, tile, &[("src.pts", layer())]);
    let b = render_with_features(expr_json, 64, 0, tile, &[("src.pts", layer())]);

    assert_eq!(a.width, b.width);
    assert_eq!(a.height, b.height);
    assert_eq!(
        a.pixels, b.pixels,
        "opaque color-expr must match constant color pixel-for-pixel"
    );
    let center = a.pixel(32, 32);
    assert!(
        center[0] > 150 && center[3] > 150,
        "center should be red: {center:?}"
    );
}
