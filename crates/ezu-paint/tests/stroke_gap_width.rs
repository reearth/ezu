//! `stroke` casings: `gap-width-px` / `gap-width-expr` (MapLibre
//! `line-gap-width`) render two parallel bands around a hole on the
//! centreline.

mod common;
use common::render_with_features;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

/// A horizontal polyline across the tile at row `y`, in extent coords.
fn line_feature(class: &str, y: i32) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("class".to_string(), Value::String(class.to_string()));
    let mut geometry = Geometry::default();
    geometry.lines.push(vec![(0, y), (4095, y)]);
    Feature {
        id: None,
        geometry,
        properties,
    }
}

fn one_line_layer(y: i32) -> FeatureLayer {
    FeatureLayer {
        name: "roads".to_string(),
        extent: 4096,
        features: vec![line_feature("x", y)],
    }
}

fn recipe(extra: &str) -> String {
    format!(
        r##"{{
      "name": "gap",
      "tile-size": 64,
      "sources": {{ "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }} }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "roads" }},
        "out":   {{ "op": "stroke", "features": "@feats", "color": "#ff0000", "width-px": 4{extra} }}
      }},
      "output": "@out"
    }}"##
    )
}

#[test]
fn gap_width_renders_an_annulus_of_the_expected_footprint() {
    // tile-size 64 over extent 4096 → 1 px per 64 extent units, so the line
    // at y=2048 sits on row 32. width 4 + gap 8 → bands on rows 24..28 and
    // 36..40, hole on 28..36.
    let r = render_with_features(
        &recipe(r#", "gap-width-px": 8"#),
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.roads", one_line_layer(2048))],
    );

    let opaque = |y: u32| r.pixel(32, y)[3] > 200;
    for y in [22, 23, 40, 41] {
        assert!(!opaque(y), "row {y} is outside the footprint");
    }
    for y in [25, 26, 37, 38] {
        assert!(opaque(y), "row {y} is inside a casing band");
    }
    for y in [29, 32, 34] {
        assert!(!opaque(y), "row {y} is inside the gap");
    }
    // Both bands together cover `2 * width` rows.
    let covered = (0..64).filter(|&y| opaque(y)).count();
    assert_eq!(covered, 8, "footprint should be two 4 px bands");
}

#[test]
fn zero_gap_is_byte_identical_to_a_plain_stroke() {
    let tile = TileId { z: 0, x: 0, y: 0 };
    let plain = render_with_features(
        &recipe(""),
        64,
        0,
        tile,
        &[("src.roads", one_line_layer(2048))],
    );
    for extra in [
        r#", "gap-width-px": 0"#,
        r#", "gap-width-expr": ["match", ["get","class"], "nope", 8, 0]"#,
    ] {
        let gapped = render_with_features(
            &recipe(extra),
            64,
            0,
            tile,
            &[("src.roads", one_line_layer(2048))],
        );
        assert_eq!(
            plain.pixels, gapped.pixels,
            "a zero gap must paint exactly like a plain stroke ({extra})"
        );
    }
    // Sanity: the plain stroke really is a 4 px band.
    let covered = (0..64).filter(|&y| plain.pixel(32, y)[3] > 200).count();
    assert_eq!(covered, 4);
}

#[test]
fn gap_width_expr_is_data_driven() {
    // `class=wide` gets a 16 px gap, `class=narrow` none; both keep width 4.
    let layer = FeatureLayer {
        name: "roads".to_string(),
        extent: 4096,
        features: vec![line_feature("narrow", 1024), line_feature("wide", 3072)],
    };
    let r = render_with_features(
        &recipe(r#", "gap-width-expr": ["match", ["get","class"], "wide", 16, 0]"#),
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.roads", layer)],
    );

    let opaque = |y: u32| r.pixel(32, y)[3] > 200;
    // Narrow line on row 16: a solid 4 px band, nothing knocked out.
    assert!(opaque(16), "narrow line should be solid at its centre");
    assert_eq!((8..24).filter(|&y| opaque(y)).count(), 4);
    // Wide line on row 48: 16 px hole, bands on rows 36..40 and 56..60.
    assert!(!opaque(48), "wide line should be hollow at its centre");
    assert!(
        opaque(37) && opaque(57),
        "wide line keeps both casing bands"
    );
    assert_eq!((32..64).filter(|&y| opaque(y)).count(), 8);
}
