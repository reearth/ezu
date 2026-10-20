//! Data-driven `fill-dabs`: `color-expr` and `opacity-expr`, evaluated per
//! feature group. A polygon filled through an expression must come out
//! exactly as the same polygon filled with the literal value its group
//! resolves to, and the world-anchored lattice must not notice that the
//! polygons were split by group.

use crate::common::render_with_features;
use ezu_features::{Feature, FeatureLayer, Geometry, Polygon, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

/// A quadrilateral with `kind = k`, its left edge at `x0`.
fn polygon_feature(k: &str, x0: i32) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("kind".to_string(), Value::String(k.to_string()));
    let mut geometry = Geometry::default();
    geometry.polygons.push(Polygon {
        exterior: vec![
            (x0, 400),
            (x0 + 2400, 700),
            (x0 + 2000, 3600),
            (x0 + 100, 3300),
            (x0, 400),
        ],
        holes: vec![],
    });
    Feature {
        id: None,
        geometry,
        properties,
    }
}

/// Render `features` through a `fill-dabs` with `dab_fields` (a JSON object
/// body without braces) on top of the jitter settings every case shares.
fn render(features: Vec<Feature>, dab_fields: &str) -> Vec<u8> {
    let json = format!(
        r##"{{
      "name": "dd-fill-dabs",
      "tile-size": 64,
      "sources": {{ "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }} }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "areas" }},
        "out":   {{ "op": "fill-dabs", "features": "@feats", "radius-px": 3, "spacing-px": 3,
                   "size-jitter": 0.4, "opacity-jitter": 0.3, "value-jitter": 0.1, {dab_fields} }}
      }},
      "output": "@out"
    }}"##
    );
    let layer = FeatureLayer {
        name: "areas".to_string(),
        extent: 4096,
        features,
    };
    render_with_features(
        &json,
        64,
        4,
        TileId { z: 4, x: 9, y: 6 },
        &[("src.areas", layer)],
    )
    .pixels
    .clone()
}

/// For each `(kind, literal)`, the expression render of that kind equals the
/// literal render, and the literals really differ from one another.
fn assert_matches_literals(expr_fields: &str, literal_fields: &[(&str, &str)]) {
    let mut seen: Vec<Vec<u8>> = Vec::new();
    for (k, lit) in literal_fields {
        let want = render(vec![polygon_feature(k, 600)], lit);
        assert!(
            want.iter().any(|&b| b != 0),
            "`{lit}` should paint something"
        );
        assert_eq!(
            render(vec![polygon_feature(k, 600)], expr_fields),
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
        r##""opacity": 0.5, "color": "#000000",
           "color-expr": ["match", ["get","kind"], "water", "#4a6c99", "#8a9a5b"]"##,
        &[
            ("water", r##""opacity": 0.5, "color": "#4a6c99""##),
            ("park", r##""opacity": 0.5, "color": "#8a9a5b""##),
        ],
    );
}

#[test]
fn opacity_expr_renders_as_each_groups_literal_opacity() {
    assert_matches_literals(
        r##""color": "#4a6c99", "opacity": 1,
           "opacity-expr": ["match", ["get","kind"], "faint", 0.2, 0.6]"##,
        &[
            ("faint", r##""color": "#4a6c99", "opacity": 0.2"##),
            ("strong", r##""color": "#4a6c99", "opacity": 0.6"##),
        ],
    );
}

#[test]
fn groups_resolving_to_the_constants_draw_what_the_constants_draw() {
    // Two overlapping polygons in different groups, filled once with
    // constants and once through expressions that give every group those
    // same constants. The dabs have to land where they did: one per lattice
    // cell, not one per group covering it.
    let feats = || vec![polygon_feature("a", 300), polygon_feature("b", 1400)];
    assert_eq!(
        render(
            feats(),
            r##""color": "#000000", "opacity": 1,
                "color-expr": ["literal", "#4a6c99"], "opacity-expr": ["+", 0.5, 0]"##
        ),
        render(feats(), r##""color": "#4a6c99", "opacity": 0.5"##),
    );
}

#[test]
fn an_overlap_takes_the_paint_of_the_later_group() {
    // Where two groups' polygons overlap, the dab there carries the paint
    // of the group drawn last, as the last-drawn polygon is on top.
    let colored = render(
        vec![polygon_feature("a", 300), polygon_feature("b", 1400)],
        r##""opacity": 0.8, "color": "#000000",
            "color-expr": ["match", ["get","kind"], "a", "#ff0000", "#0000ff"]"##,
    );
    // The middle of the overlap: canvas x ≈ 4 + 64 × 2000 / 4096, y ≈ 36.
    let at = ((36 * 72 + 35) * 4) as usize;
    let px = &colored[at..at + 4];
    assert!(
        px[3] > 0 && px[2] > px[0],
        "the overlap should be blue: {px:?}"
    );
}
