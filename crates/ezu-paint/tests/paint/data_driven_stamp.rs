//! Data-driven `stamp` paint: a `scale-expr` MapLibre number expression
//! evaluated per feature group scales the sprite differently per feature.

use crate::common::{disk_sprite, render_with_features_and_images};
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

/// A single point feature at extent coords `(x, y)` with size-class `c`.
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

#[test]
fn scale_expr_sizes_the_stamp_per_feature() {
    // Two points: `c=small` (left) and `c=big` (right). A
    // `["match", ["get","c"], "big", 2, 0.5]` scale-expr stamps the right
    // point at 4× the linear size of the left, so the right half of the
    // canvas ends up with many more opaque pixels than the left.
    let layer = FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features: vec![
            point_feature("small", 1024, 2048),
            point_feature("big", 3072, 2048),
        ],
    };

    let json = r##"{
      "name": "dd-stamp",
      "tile-size": 64,
      "sources": {
        "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" },
        "dot": { "type": "image", "src": "builtin:dot" }
      },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "pts" },
        "img":   { "op": "image", "src": "@dot" },
        "out":   { "op": "stamp", "features": "@feats", "image": "@img",
                   "scale-expr": ["match", ["get","c"], "big", 2.0, 0.5] }
      },
      "output": "@out"
    }"##;

    // A 24×24 sprite with a centered opaque red disk (radius 10).
    let sprite = disk_sprite(24, 24, 10.0, [255, 0, 0, 255]);
    let r = render_with_features_and_images(
        json,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer)],
        &[("dot", sprite)],
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
    let left = opaque_in(0, 32); // small point, ~px 16
    let right = opaque_in(32, 64); // big point, ~px 48
    assert!(left > 0, "small stamp should paint something: {left}");
    assert!(
        right > left * 3,
        "big stamp ({right} px) should dwarf the small one ({left} px)"
    );
}
