//! Data-driven `line`: `color-expr`, `radius-expr` and `opacity-expr`, each
//! evaluated per feature group. A feature stroked through an expression must
//! come out exactly as the same feature stroked with the literal value its
//! group resolves to, and the brush's world-seeded jitter must not notice
//! that the lines were split by group.

use crate::common::render_with_features;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

/// A polyline with `kind = k`. `slope` tilts it, so two lines of different
/// slopes cross.
fn line_feature(k: &str, slope: i32) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("kind".to_string(), Value::String(k.to_string()));
    let mut geometry = Geometry::default();
    geometry.lines.push(vec![
        (200, 2048 - slope),
        (1500, 2048 - slope / 3),
        (2600, 2048 + slope / 3),
        (3900, 2048 + slope),
    ]);
    Feature {
        id: None,
        geometry,
        properties,
    }
}

/// Render `features` through a `line` with `line_fields` (a JSON object body
/// without braces) and a soft round brush.
fn render(features: Vec<Feature>, line_fields: &str) -> Vec<u8> {
    let json = format!(
        r##"{{
      "name": "dd-line",
      "tile-size": 64,
      "sources": {{ "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }} }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "roads" }},
        "brush": {{ "op": "brush-solid", "width-px": 3, "color": "#000000" }},
        "out":   {{ "op": "line", "features": "@feats", "brush": "@brush", {line_fields} }}
      }},
      "output": "@out"
    }}"##
    );
    let layer = FeatureLayer {
        name: "roads".to_string(),
        extent: 4096,
        features,
    };
    render_with_features(
        &json,
        64,
        4,
        TileId { z: 3, x: 5, y: 2 },
        &[("src.roads", layer)],
    )
    .pixels
    .clone()
}

/// For each `(kind, literal)`, the expression render of that kind equals the
/// literal render, and the literals really differ from one another.
fn assert_matches_literals(expr_fields: &str, literal_fields: &[(&str, &str)]) {
    let mut seen: Vec<Vec<u8>> = Vec::new();
    for (k, lit) in literal_fields {
        let want = render(vec![line_feature(k, 600)], lit);
        assert!(
            want.iter().any(|&b| b != 0),
            "`{lit}` should paint something"
        );
        assert_eq!(
            render(vec![line_feature(k, 600)], expr_fields),
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
fn color_expr_renders_as_each_groups_literal_color() {
    assert_matches_literals(
        r##""color": "#000000",
           "color-expr": ["match", ["get","kind"], "river", "#3366cc", "#cc3333"]"##,
        &[
            ("river", r##""color": "#3366cc""##),
            ("road", r##""color": "#cc3333""##),
        ],
    );
}

#[test]
fn radius_expr_renders_as_each_groups_literal_radius() {
    assert_matches_literals(
        r##""color": "#202020", "radius-px": 1,
           "radius-expr": ["match", ["get","kind"], "thin", 1.5, 4]"##,
        &[
            ("thin", r##""color": "#202020", "radius-px": 1.5"##),
            ("thick", r##""color": "#202020", "radius-px": 4"##),
        ],
    );
}

#[test]
fn radius_expr_follows_zoom() {
    // The render is at z3: the curve gives 2 + 3 × 0.5 = 3.5 px.
    assert_eq!(
        render(
            vec![line_feature("a", 600)],
            r##""color": "#202020", "radius-expr": ["interpolate", ["linear"], ["zoom"], 0, 2, 10, 7]"##
        ),
        render(
            vec![line_feature("a", 600)],
            r##""color": "#202020", "radius-px": 3.5"##
        ),
    );
}

#[test]
fn opacity_expr_renders_as_each_groups_literal_opacity() {
    assert_matches_literals(
        r##""color": "#202020", "opacity": 1,
           "opacity-expr": ["match", ["get","kind"], "faint", 0.2, 0.7]"##,
        &[
            ("faint", r##""color": "#202020", "opacity": 0.2"##),
            ("strong", r##""color": "#202020", "opacity": 0.7"##),
        ],
    );
}

#[test]
fn groups_resolving_to_the_constants_draw_what_the_constants_draw() {
    // Two crossing lines in different groups, stroked once with constants
    // and once through expressions that give every group those same
    // constants. Splitting the lines by group must not change where the
    // brush puts its dabs or how crossing strokes mix.
    let feats = || vec![line_feature("a", 900), line_feature("b", -900)];
    assert_eq!(
        render(
            feats(),
            r##""color": "#000000", "radius-px": 1,
                "color-expr": ["literal", "#335577"],
                "radius-expr": ["+", 2.5, 0],
                "opacity-expr": ["+", 0.6, 0]"##
        ),
        render(
            feats(),
            r##""color": "#335577", "radius-px": 2.5, "opacity": 0.6"##
        ),
    );
}
