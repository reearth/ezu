//! Data-driven `stroke` paint: `color-expr` / `width-expr` MapLibre
//! expressions evaluated per feature group, plus color-conversion parity
//! with the constant `color` path.

use crate::common::render_with_features;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

/// A horizontal polyline across the tile at row `y`, in extent coords.
fn hline(y: i32) -> Vec<(i32, i32)> {
    vec![(0, y), (4095, y)]
}

fn line_feature(class: &str, y: i32) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("class".to_string(), Value::String(class.to_string()));
    let mut geometry = Geometry::default();
    geometry.lines.push(hline(y));
    Feature {
        id: None,
        geometry,
        properties,
    }
}

fn one_line_layer(name: &str, class: &str, y: i32) -> FeatureLayer {
    FeatureLayer {
        name: name.to_string(),
        extent: 4096,
        features: vec![line_feature(class, y)],
    }
}

#[test]
fn color_expr_matches_per_feature_class() {
    // Two horizontal lines, `class=a` (top) and `class=b` (bottom). A
    // `["match", ["get","class"], ...]` color-expr strokes each its own color.
    let layer = FeatureLayer {
        name: "roads".to_string(),
        extent: 4096,
        features: vec![line_feature("a", 1024), line_feature("b", 3072)],
    };

    let json = r##"{
      "name": "dd-stroke",
      "tile-size": 64,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "roads" },
        "out":   { "op": "stroke", "features": "@feats", "color": "#000000", "width-px": 6,
                   "color-expr": ["match", ["get","class"], "a", "#ff0000", "b", "#00ff00", "#000000"] }
      },
      "output": "@out"
    }"##;

    let r = render_with_features(
        json,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.roads", layer)],
    );

    // Top line (~y=16) is red; bottom line (~y=48) is green.
    let top = r.pixel(32, 16);
    assert!(
        top[0] > 150 && top[1] < 80 && top[3] > 150,
        "top line should be red: {top:?}"
    );
    let bottom = r.pixel(32, 48);
    assert!(
        bottom[1] > 150 && bottom[0] < 80 && bottom[3] > 150,
        "bottom line should be green: {bottom:?}"
    );
}

#[test]
fn width_expr_drives_stroke_thickness() {
    // A `["match", ["get","class"], "thin", 2, 12]` width-expr makes a
    // `class=thick` line noticeably wider than a `class=thin` one.
    let layer = FeatureLayer {
        name: "roads".to_string(),
        extent: 4096,
        features: vec![line_feature("thin", 1024), line_feature("thick", 3072)],
    };

    let json = r##"{
      "name": "dd-width",
      "tile-size": 64,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "roads" },
        "out":   { "op": "stroke", "features": "@feats", "color": "#ffffff", "width-px": 1,
                   "width-expr": ["match", ["get","class"], "thin", 2, 12] }
      },
      "output": "@out"
    }"##;

    let r = render_with_features(
        json,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.roads", layer)],
    );

    // Count opaque rows in a vertical column through each line: the thick
    // line (12px) should cover more rows than the thin one (2px).
    let count_rows =
        |cx: u32, y_lo: u32, y_hi: u32| (y_lo..y_hi).filter(|&y| r.pixel(cx, y)[3] > 100).count();
    let thin_rows = count_rows(32, 8, 24); // around y=16
    let thick_rows = count_rows(32, 40, 56); // around y=48
    assert!(
        thick_rows > thin_rows,
        "thick line ({thick_rows} rows) should be wider than thin ({thin_rows} rows)"
    );
}

#[test]
fn opaque_color_expr_matches_constant_color_pixel_for_pixel() {
    // Color-conversion parity: a data-driven opaque color must stroke the
    // exact same pixels as the constant `color` of the same color.
    let constant_json = r##"{
      "name": "const",
      "tile-size": 64,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "roads" },
        "out":   { "op": "stroke", "features": "@feats", "color": "#ff0000", "width-px": 6 }
      },
      "output": "@out"
    }"##;

    let expr_json = r##"{
      "name": "expr",
      "tile-size": 64,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "roads" },
        "out":   { "op": "stroke", "features": "@feats", "color": "#000000", "width-px": 6,
                   "color-expr": ["match", ["get","class"], "nope", "#000000", "#ff0000"] }
      },
      "output": "@out"
    }"##;

    let tile = TileId { z: 0, x: 0, y: 0 };
    let a = render_with_features(
        constant_json,
        64,
        0,
        tile,
        &[("src.roads", one_line_layer("roads", "x", 2048))],
    );
    let b = render_with_features(
        expr_json,
        64,
        0,
        tile,
        &[("src.roads", one_line_layer("roads", "x", 2048))],
    );

    assert_eq!(a.width, b.width);
    assert_eq!(a.height, b.height);
    assert_eq!(
        a.pixels, b.pixels,
        "data-driven opaque color-expr must match constant color pixel-for-pixel"
    );
    // Sanity: the line really is red.
    let mid = a.pixel(32, 32);
    assert!(mid[0] > 150 && mid[3] > 150, "line should be red: {mid:?}");
}

/// Render one horizontal `class` line at mid-tile through a `stroke` with
/// `fields` (a JSON object body without braces).
fn render_line(class: &str, fields: &str) -> Vec<u8> {
    let json = format!(
        r##"{{
      "name": "dash-unit",
      "tile-size": 64,
      "sources": {{ "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }} }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "roads" }},
        "out":   {{ "op": "stroke", "features": "@feats", "color": "#000000", {fields} }}
      }},
      "output": "@out"
    }}"##
    );
    render_with_features(
        &json,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.roads", one_line_layer("roads", class, 2048))],
    )
    .pixels
    .clone()
}

#[test]
fn dash_unit_width_scales_the_dashes_by_each_groups_width() {
    // In widths, a [2, 1] dash on a 2 px stroke is [4, 2] px and on a 4 px
    // stroke [8, 4] px — whether the width is a constant or a `width-expr`
    // resolving per feature.
    let expr = r#""width-px": 1, "width-expr": ["match", ["get","class"], "thin", 2, 4],
                  "dasharray": [2, 1], "dash-unit": "width""#;
    let thin = render_line("thin", r#""width-px": 2, "dasharray": [4, 2]"#);
    let wide = render_line("wide", r#""width-px": 4, "dasharray": [8, 4]"#);
    assert_ne!(thin, wide);
    assert_eq!(render_line("thin", expr), thin);
    assert_eq!(render_line("wide", expr), wide);
    assert_eq!(
        render_line(
            "wide",
            r#""width-px": 4, "dasharray": [2, 1], "dash-unit": "width""#
        ),
        wide
    );

    // `px`, the default, keeps the lengths whatever the width.
    assert_eq!(
        render_line(
            "wide",
            r#""width-px": 4, "dasharray": [4, 2], "dash-unit": "px""#
        ),
        render_line("wide", r#""width-px": 4, "dasharray": [4, 2]"#),
    );
}

#[test]
fn offset_draws_the_line_beside_itself() {
    // East-bound at mid-tile; 6 px to the right of travel is 6 px down,
    // where the same line drawn 6 px lower lands (64 extent units per px).
    let at = |y: i32, fields: &str| {
        let json = format!(
            r##"{{
      "name": "offset",
      "tile-size": 64,
      "sources": {{ "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }} }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "roads" }},
        "out":   {{ "op": "stroke", "features": "@feats", "color": "#000000", "width-px": 2 {fields} }}
      }},
      "output": "@out"
    }}"##
        );
        render_with_features(
            &json,
            64,
            0,
            TileId { z: 0, x: 0, y: 0 },
            &[("src.roads", one_line_layer("roads", "minor", y))],
        )
        .pixels
        .clone()
    };
    let lower = at(2048 + 6 * 64, "");
    assert_ne!(lower, at(2048, ""));
    assert_eq!(at(2048, r#", "offset-px": 6"#), lower);
    assert_eq!(
        at(
            2048,
            r#", "offset-expr": ["match", ["get", "class"], "minor", 6, 0]"#
        ),
        lower
    );
    // Negative goes left of travel: up.
    assert_eq!(at(2048, r#", "offset-px": -6"#), at(2048 - 6 * 64, ""));
}
