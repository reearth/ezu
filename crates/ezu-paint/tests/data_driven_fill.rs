//! Data-driven `fill-solid` fills: a `fill-expr` MapLibre color expression
//! evaluated per feature group, plus color-conversion parity with the
//! constant `fill` path.

mod common;
use common::render_with_features;
use ezu_features::{Feature, FeatureLayer, Geometry, Polygon, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

/// A rectangular polygon `[x0, x1] × [y0, y1]` in tile-local extent coords.
fn rect(x0: i32, y0: i32, x1: i32, y1: i32) -> Polygon {
    Polygon {
        exterior: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)],
        holes: vec![],
    }
}

fn feature_with(class: &str, poly: Polygon) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("class".to_string(), Value::String(class.to_string()));
    let mut geometry = Geometry::default();
    geometry.polygons.push(poly);
    Feature {
        id: None,
        geometry,
        properties,
    }
}

/// A layer of one feature holding `poly` with a single string property.
fn one_feature_layer(name: &str, key: &str, val: &str, poly: Polygon) -> FeatureLayer {
    let mut properties = HashMap::new();
    properties.insert(key.to_string(), Value::String(val.to_string()));
    let mut geometry = Geometry::default();
    geometry.polygons.push(poly);
    FeatureLayer {
        name: name.to_string(),
        extent: 4096,
        features: vec![Feature {
            id: None,
            geometry,
            properties,
        }],
    }
}

#[test]
fn fill_expr_matches_per_feature_class() {
    // Two half-tile polygons carrying `class=a` (left) and `class=b` (right).
    // A `["match", ["get","class"], ...]` fill paints each its own color:
    // left red, right green.
    let layer = FeatureLayer {
        name: "poly".to_string(),
        extent: 4096,
        features: vec![
            feature_with("a", rect(0, 0, 2048, 4095)),
            feature_with("b", rect(2048, 0, 4095, 4095)),
        ],
    };

    let json = r##"{
      "name": "data-driven",
      "tile-size": 32,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "poly" },
        "out":   { "op": "fill-solid", "features": "@feats", "fill": "#000000",
                   "fill-expr": ["match", ["get", "class"], "a", "#ff0000", "b", "#00ff00", "#000000"] }
      },
      "output": "@out"
    }"##;

    let r = render_with_features(
        json,
        32,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.poly", layer)],
    );

    // Left half (x=8) is red; right half (x=24) is green.
    let left = r.pixel(8, 16);
    assert!(
        left[0] > 200 && left[1] < 60 && left[3] > 200,
        "left half should be red: {left:?}"
    );
    let right = r.pixel(24, 16);
    assert!(
        right[1] > 200 && right[0] < 60 && right[3] > 200,
        "right half should be green: {right:?}"
    );
}

#[test]
fn opaque_fill_expr_matches_constant_fill_pixel_for_pixel() {
    // Color-conversion parity: a data-driven opaque color must paint the
    // exact same pixels as the constant `fill` of the same color.
    let poly = rect(0, 0, 4095, 4095);

    let constant_json = r##"{
      "name": "const",
      "tile-size": 32,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "poly" },
        "out":   { "op": "fill-solid", "features": "@feats", "fill": "#ff0000" }
      },
      "output": "@out"
    }"##;

    let expr_json = r##"{
      "name": "expr",
      "tile-size": 32,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "poly" },
        "out":   { "op": "fill-solid", "features": "@feats", "fill": "#000000",
                   "fill-expr": ["match", ["get", "class"], "nope", "#000000", "#ff0000"] }
      },
      "output": "@out"
    }"##;

    let tile = TileId { z: 0, x: 0, y: 0 };
    let a = render_with_features(
        constant_json,
        32,
        0,
        tile,
        &[(
            "src.poly",
            one_feature_layer("poly", "class", "x", poly.clone()),
        )],
    );
    let b = render_with_features(
        expr_json,
        32,
        0,
        tile,
        &[("src.poly", one_feature_layer("poly", "class", "x", poly))],
    );

    // Every pixel must match exactly.
    assert_eq!(a.width, b.width);
    assert_eq!(a.height, b.height);
    assert_eq!(
        a.pixels, b.pixels,
        "data-driven opaque fill must match constant fill pixel-for-pixel"
    );
    // Sanity: the interior really is red.
    let mid = a.pixel(16, 16);
    assert!(
        mid[0] > 200 && mid[3] > 200,
        "interior should be red: {mid:?}"
    );
}
