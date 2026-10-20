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

/// Render one point of class `c` at extent `(x, y)` through a `stamp` of a
/// 12×12 red disk with `fields` (a JSON object body without braces, possibly
/// empty).
fn render_one(c: &str, x: i32, y: i32, fields: &str) -> std::sync::Arc<ezu_graph::RasterBuf> {
    let sep = if fields.is_empty() { "" } else { "," };
    let json = format!(
        r##"{{
      "name": "stamp-one",
      "tile-size": 64,
      "sources": {{
        "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "dot": {{ "type": "image", "src": "builtin:dot" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        "img":   {{ "op": "image", "src": "@dot" }},
        "out":   {{ "op": "stamp", "features": "@feats", "image": "@img"{sep} {fields} }}
      }},
      "output": "@out"
    }}"##
    );
    let layer = FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features: vec![point_feature(c, x, y)],
    };
    render_with_features_and_images(
        &json,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer)],
        &[("dot", disk_sprite(12, 12, 6.0, [255, 0, 0, 255]))],
    )
}

/// Rows of the canvas with any paint in them.
fn painted_rows(r: &ezu_graph::RasterBuf) -> Vec<u32> {
    (0..r.height)
        .filter(|&y| (0..r.width).any(|x| r.pixel(x, y)[3] > 0))
        .collect()
}

#[test]
fn bottom_anchor_sets_the_sprite_on_the_point() {
    // The point is canvas row 32. Centred, the disk spans it; anchored at
    // its bottom, the disk sits wholly above it.
    let centred = painted_rows(&render_one("x", 2048, 2048, ""));
    assert!(
        centred.contains(&26) && centred.contains(&37),
        "{centred:?}"
    );
    let bottom = painted_rows(&render_one("x", 2048, 2048, r#""anchor": "bottom""#));
    assert_eq!(bottom.first(), Some(&20), "{bottom:?}");
    assert_eq!(bottom.last(), Some(&31), "{bottom:?}");
}

#[test]
fn anchor_expr_renders_as_each_groups_literal_anchor() {
    for (c, anchor) in [("pin", "bottom"), ("flag", "top-left")] {
        let want = render_one("x", 2048, 2048, &format!(r#""anchor": "{anchor}""#));
        assert_eq!(
            render_one(
                c,
                2048,
                2048,
                r#""anchor-expr": ["match", ["get","c"], "pin", "bottom", "top-left"]"#
            )
            .pixels,
            want.pixels,
            "class `{c}` should be anchored `{anchor}`"
        );
    }
    // A name that is not an anchor falls back to `anchor`.
    assert_eq!(
        render_one(
            "x",
            2048,
            2048,
            r#""anchor": "left", "anchor-expr": "nowhere""#
        )
        .pixels,
        render_one("x", 2048, 2048, r#""anchor": "left""#).pixels,
    );
}

#[test]
fn offset_shifts_the_sprite_in_scaled_pixels() {
    // 64 extent units are one canvas pixel. An offset of [6, -4] px lands
    // the sprite where a point 6 px right and 4 px up would.
    assert_eq!(
        render_one("x", 2048, 2048, r#""offset": [6, -4]"#).pixels,
        render_one("x", 2048 + 6 * 64, 2048 - 4 * 64, "").pixels,
    );
    // At `scale: 2` the offset doubles with the sprite.
    assert_eq!(
        render_one("x", 2048, 2048, r#""scale": 2, "offset": [3, 0]"#).pixels,
        render_one("x", 2048 + 6 * 64, 2048, r#""scale": 2"#).pixels,
    );
}

#[test]
fn an_anchored_sprite_turns_about_the_point() {
    // Anchored at its bottom and turned half a turn, the disk hangs below
    // the point instead of standing above it.
    let rows = painted_rows(&render_one(
        "x",
        2048,
        2048,
        r#""anchor": "bottom", "rotation-deg": 180"#,
    ));
    assert_eq!(rows.first(), Some(&32), "{rows:?}");
    assert_eq!(rows.last(), Some(&43), "{rows:?}");
}
