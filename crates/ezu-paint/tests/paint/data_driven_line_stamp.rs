//! Data-driven `line-stamp`: `width-expr`, `opacity-expr` and a pattern
//! named per feature (`sprite` + `name-expr`), each evaluated per feature
//! group. A feature drawn through an expression must come out exactly as the
//! same feature drawn with the literal value its group resolves to.

use crate::common::render_with_features_and_sprite;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::{RasterBuf, SpriteRect, SpriteSheet, TileId};
use std::collections::HashMap;

/// A diagonal polyline with `kind = k`, so the stamps rotate and sample
/// between pixels rather than landing on a lucky grid.
fn line_feature(k: &str) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("kind".to_string(), Value::String(k.to_string()));
    let mut geometry = Geometry::default();
    geometry
        .lines
        .push(vec![(300, 500), (2000, 1800), (3900, 3500)]);
    Feature {
        id: None,
        geometry,
        properties,
    }
}

/// An atlas with two icons: `red` (8×8) and `blue` (8×4), so a fit to the
/// stroke width scales them differently.
fn two_icon_sheet() -> SpriteSheet {
    let mut atlas = RasterBuf::new(16, 8);
    for y in 0..8u32 {
        for x in 0..16u32 {
            let i = ((y * 16 + x) * 4) as usize;
            let color = if x < 8 {
                [255u8, 0, 0, 255]
            } else if y < 4 {
                [0, 0, 255, 255]
            } else {
                [0, 0, 0, 0]
            };
            atlas.pixels[i..i + 4].copy_from_slice(&color);
        }
    }
    let rect = |x, width, height| SpriteRect {
        x,
        y: 0,
        width,
        height,
        pixel_ratio: 1.0,
        ..SpriteRect::default()
    };
    let mut icons = HashMap::new();
    icons.insert("red".to_string(), rect(0, 8, 8));
    icons.insert("blue".to_string(), rect(8, 8, 4));
    SpriteSheet { atlas, icons }
}

/// Render one `kind = k` line through a `line-stamp` whose fields are
/// `stamp_fields` (a JSON object body without braces). `pat` is an `icon`
/// node cropping `red` from the sheet, for stamps that take an `image`.
fn render(k: &str, stamp_fields: &str) -> Vec<u8> {
    render_with_pat(k, "red", stamp_fields)
}

/// [`render`] with the `pat` node cropping `pat_icon`.
fn render_with_pat(k: &str, pat_icon: &str, stamp_fields: &str) -> Vec<u8> {
    let json = format!(
        r##"{{
      "name": "dd-line-stamp",
      "tile-size": 64,
      "sources": {{
        "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "sheet": {{ "type": "sprite", "image": "builtin:atlas",
                   "index": {{ "red":  {{"x":0,"y":0,"width":8,"height":8}},
                              "blue": {{"x":8,"y":0,"width":8,"height":4}} }} }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "roads" }},
        "pat":   {{ "op": "icon", "sprite": "@sheet", "name": "{pat_icon}" }},
        "out":   {{ "op": "line-stamp", "features": "@feats", {stamp_fields} }}
      }},
      "output": "@out"
    }}"##
    );
    let layer = FeatureLayer {
        name: "roads".to_string(),
        extent: 4096,
        features: vec![line_feature(k)],
    };
    render_with_features_and_sprite(
        &json,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.roads", layer)],
        "atlas",
        two_icon_sheet(),
    )
    .pixels
    .clone()
}

/// For each `(kind, literal)`, the expression render of that kind equals the
/// literal render, and the literals really differ from one another.
fn assert_matches_literals(expr_fields: &str, literal_fields: &[(&str, String)]) {
    let mut seen: Vec<Vec<u8>> = Vec::new();
    for (k, lit) in literal_fields {
        let want = render(k, lit);
        assert!(
            want.iter().any(|&b| b != 0),
            "`{lit}` should paint something"
        );
        assert_eq!(
            render(k, expr_fields),
            want,
            "kind `{k}` should render as `{lit}`"
        );
        assert!(
            !seen.contains(&want),
            "the literals should differ, or the test proves nothing"
        );
        seen.push(want);
    }
}

#[test]
fn width_expr_renders_as_each_groups_literal_width() {
    assert_matches_literals(
        r#""image": "@pat", "width-px": 1,
           "width-expr": ["match", ["get","kind"], "thin", 3, 9]"#,
        &[
            ("thin", r#""image": "@pat", "width-px": 3"#.to_string()),
            ("thick", r#""image": "@pat", "width-px": 9"#.to_string()),
        ],
    );
}

#[test]
fn width_expr_follows_zoom() {
    // A zoom curve: this is what MapLibre `line-width` usually is.
    let fields =
        r#""image": "@pat", "width-expr": ["interpolate", ["linear"], ["zoom"], 0, 6, 10, 20]"#;
    assert_eq!(
        render("a", fields),
        render("a", r#""image": "@pat", "width-px": 6"#)
    );
}

#[test]
fn opacity_expr_renders_as_each_groups_literal_opacity() {
    assert_matches_literals(
        r#""image": "@pat", "width-px": 6, "opacity": 1,
           "opacity-expr": ["match", ["get","kind"], "faint", 0.25, 0.75]"#,
        &[
            (
                "faint",
                r#""image": "@pat", "width-px": 6, "opacity": 0.25"#.to_string(),
            ),
            (
                "strong",
                r#""image": "@pat", "width-px": 6, "opacity": 0.75"#.to_string(),
            ),
        ],
    );
}

#[test]
fn name_expr_repeats_each_groups_own_pattern() {
    // `name-expr` crops each feature's pattern from the sheet; the result is
    // the same line stamped with that icon wired in through an `icon` node.
    for icon in ["red", "blue"] {
        let want = render_with_pat(icon, icon, r#""image": "@pat", "width-px": 6"#);
        assert!(want.iter().any(|&b| b != 0), "`{icon}` should paint");
        assert_eq!(
            render(
                icon,
                r#""sprite": "@sheet", "name-expr": ["get","kind"], "width-px": 6"#
            ),
            want,
            "kind `{icon}` should stamp the `{icon}` icon"
        );
    }
}
