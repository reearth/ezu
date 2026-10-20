//! `text` / `text-labels` layout `*-expr` fields: `justify-expr`,
//! `offset-em-expr`, `max-width-em-expr`, `letter-spacing-em-expr`,
//! `line-height-expr` and `transform-expr`, each evaluated per feature group
//! in place of its constant. Read through `label-boxes`, whose text boxes show
//! how each label was laid out, and the render itself.

use crate::common::features_of_node;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::{build_graph, RasterBuf, TileId};
use ezu_paint::nodes::{default_registry, FilteredFeatures};
use ezu_style::Document;
use std::collections::HashMap;
use std::sync::Arc;

const TILE: u32 = 128;
const UNITS_PER_PX: f32 = 4096.0 / TILE as f32;

fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

/// A feature labelled `name`, carrying a `kind` property for the
/// expressions to pick on.
fn feature(name: &str, kind: &str, geometry: Geometry) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), Value::String(name.to_string()));
    properties.insert("kind".to_string(), Value::String(kind.to_string()));
    Feature {
        id: None,
        geometry,
        properties,
    }
}

fn point(name: &str, kind: &str, x: i32, y: i32) -> Feature {
    let mut geometry = Geometry::default();
    geometry.points.push((x, y));
    feature(name, kind, geometry)
}

/// 16 px labels through `text-labels` → `label-placement`, drawn by
/// `text-draw` over their `label-boxes` of the given `parts`. `extra` adds
/// fields to the label layer.
fn recipe_with(parts: &str, extra: &str) -> String {
    format!(
        r##"{{
      "name": "layout-exprs",
      "tile-size": {TILE},
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats":  {{ "op": "features", "source": "src", "layer": "pts" }},
        "labels": {{ "op": "text-labels", "features": "@feats", "source": "src", "layer": "pts",
                     "font": ["body"], "text": ["get", "name"], "size": 16,
                     "collide": false, "color": "#ffffff" {extra} }},
        "placed": {{ "op": "label-placement", "labels": ["@labels"] }},
        "boxes":  {{ "op": "label-boxes", "placement": "@placed", "parts": [{parts}] }},
        "fill":   {{ "op": "fill-solid", "features": "@boxes", "fill": "#0000ff" }},
        "text":   {{ "op": "text-draw", "labels": "@labels", "placement": "@placed" }},
        "out":    {{ "op": "stack", "layers": ["@fill", "@text"] }}
      }},
      "output": "@out"
    }}"##,
        font = font_url(),
    )
}

fn recipe(extra: &str) -> String {
    recipe_with(r#""text""#, extra)
}

fn run(recipe: &str, features: Vec<Feature>) -> (Arc<FilteredFeatures>, Arc<RasterBuf>) {
    let layer = FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features,
    };
    features_of_node(
        recipe,
        TILE,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer)],
        "boxes",
    )
}

/// The build-time warnings of a style, or its build error.
fn warnings(recipe: &str) -> Result<Vec<String>, String> {
    let doc = Document::from_json(recipe).map_err(|e| e.to_string())?;
    build_graph(&doc, &default_registry())
        .map(|g| g.warnings().to_vec())
        .map_err(|e| e.to_string())
}

type Box4 = (f32, f32, f32, f32);

/// Each box `(min_x, min_y, max_x, max_y)` in px with its feature's `kind`,
/// ordered by kind then position.
fn boxes(feats: &FilteredFeatures) -> Vec<(String, Box4)> {
    let mut v: Vec<_> = feats
        .groups
        .iter()
        .flat_map(|g| {
            let kind = match g.properties.get("kind") {
                Some(maplibre_expr::Value::String(s)) => s.clone(),
                _ => String::new(),
            };
            g.polygons.iter().map(move |p| {
                let b = p.exterior.iter().fold(
                    (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
                    |(x0, y0, x1, y1), &(x, y)| {
                        let (x, y) = (x as f32 / UNITS_PER_PX, y as f32 / UNITS_PER_PX);
                        (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
                    },
                );
                (kind.clone(), b)
            })
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1 .0.total_cmp(&b.1 .0)));
    v
}

fn width((x0, _, x1, _): Box4) -> f32 {
    x1 - x0
}

fn height((_, y0, _, y1): Box4) -> f32 {
    y1 - y0
}

/// One label of wrappable text at the tile centre.
fn centre_label() -> Vec<Feature> {
    vec![point("aa bb cc", "x", 2048, 2048)]
}

/// Each layout field with a value that changes the layout of
/// [`centre_label`], and an expression yielding the same value.
const CASES: &[(&str, &str, &str)] = &[
    (
        "justify",
        r#""justify": "right", "max-width-em": 2"#,
        r#""justify-expr": ["literal", "right"], "max-width-em": 2"#,
    ),
    (
        "offset-em",
        r#""offset-em": [1.5, -0.75]"#,
        r#""offset-em-expr": ["literal", [1.5, -0.75]]"#,
    ),
    (
        "max-width-em",
        r#""max-width-em": 2"#,
        // A zoom curve, the common shape: 2 at this tile's zoom.
        r#""max-width-em-expr": ["interpolate", ["linear"], ["zoom"], 0, 2, 10, 8]"#,
    ),
    (
        "letter-spacing-em",
        r#""letter-spacing-em": 0.25"#,
        r#""letter-spacing-em-expr": ["literal", 0.25]"#,
    ),
    (
        "line-height",
        r#""line-height": 2, "max-width-em": 2"#,
        r#""line-height-expr": ["literal", 2], "max-width-em": 2"#,
    ),
    (
        "transform",
        r#""transform": "uppercase""#,
        r#""transform-expr": ["literal", "uppercase"]"#,
    ),
];

#[test]
fn each_expression_lays_out_like_the_constant_it_yields() {
    let default = run(&recipe(""), centre_label());
    for &(name, constant, expr) in CASES {
        let c = run(&recipe(&format!(", {constant}")), centre_label());
        let e = run(&recipe(&format!(", {expr}")), centre_label());
        assert!(
            c.1.pixels != default.1.pixels,
            "{name}: the constant should change the render"
        );
        assert_eq!(boxes(&c.0), boxes(&e.0), "{name}");
        assert!(c.1.pixels == e.1.pixels, "{name}: the renders differ");
        assert!(
            warnings(&recipe(&format!(", {expr}")))
                .expect("builds")
                .is_empty(),
            "{name}"
        );
    }
}

#[test]
fn groups_with_the_same_text_lay_out_by_their_own_values() {
    // Both labels read "aa bb cc" at the same size, so only the expression
    // results tell their layouts apart; a block shared between them would
    // give both the same box.
    let feats = || {
        vec![
            point("aa bb cc", "narrow", 2048, 1024),
            point("aa bb cc", "wide", 2048, 3072),
        ]
    };
    let by_kind = |narrow: &str, wide: &str| {
        format!(r#"["match", ["get", "kind"], "narrow", {narrow}, {wide}]"#)
    };

    let wrap = run(
        &recipe(&format!(r#", "max-width-em-expr": {}"#, by_kind("2", "20"))),
        feats(),
    );
    let got = boxes(&wrap.0);
    assert_eq!(got.len(), 2, "{got:?}");
    let (narrow, wide) = (got[0].1, got[1].1);
    assert!(height(narrow) > 1.5 * height(wide), "{got:?}");
    assert!(width(wide) > width(narrow), "{got:?}");

    let spaced = run(
        &recipe(&format!(
            r#", "letter-spacing-em-expr": {}"#,
            by_kind("0", "0.5")
        )),
        feats(),
    );
    let got = boxes(&spaced.0);
    assert!(width(got[1].1) > width(got[0].1) + 16.0, "{got:?}");

    let shifted = run(
        &recipe(&format!(
            r#", "offset-em-expr": {}"#,
            by_kind(r#"["literal", [0, 0]]"#, r#"["literal", [2, 0]]"#)
        )),
        feats(),
    );
    let got = boxes(&shifted.0);
    assert!((got[1].1 .0 - got[0].1 .0 - 32.0).abs() < 0.5, "{got:?}");

    let upper = run(
        &recipe(&format!(
            r#", "transform-expr": {}"#,
            by_kind(r#""none""#, r#""uppercase""#)
        )),
        feats(),
    );
    let got = boxes(&upper.0);
    assert!(width(got[1].1) > width(got[0].1), "{got:?}");

    // Justification moves lines inside an unchanged box, so the render
    // tells it: one label justified each way matches neither constant.
    let justified = run(
        &recipe(&format!(
            r#", "max-width-em": 2, "justify-expr": {}"#,
            by_kind(r#""left""#, r#""right""#)
        )),
        feats(),
    );
    for constant in ["left", "right"] {
        let all = run(
            &recipe(&format!(r#", "max-width-em": 2, "justify": "{constant}""#)),
            feats(),
        );
        assert_eq!(boxes(&justified.0), boxes(&all.0));
        assert!(justified.1.pixels != all.1.pixels, "{constant}");
    }

    let tall = run(
        &recipe(&format!(
            r#", "max-width-em": 2, "line-height-expr": {}"#,
            by_kind("1.2", "2.4")
        )),
        feats(),
    );
    let got = boxes(&tall.0);
    assert!(height(got[1].1) > height(got[0].1) + 16.0, "{got:?}");
}

#[test]
fn a_result_that_names_no_value_falls_back_to_the_constant() {
    let plain = run(
        &recipe(r#", "max-width-em": 2, "justify": "right", "transform": "uppercase""#),
        centre_label(),
    );
    let fallback = run(
        &recipe(
            r#", "max-width-em": 2, "justify": "right", "transform": "uppercase",
               "justify-expr": ["literal", "middle"], "transform-expr": ["literal", "shout"]"#,
        ),
        centre_label(),
    );
    assert_eq!(boxes(&plain.0), boxes(&fallback.0));
    assert!(plain.1.pixels == fallback.1.pixels, "the renders differ");
}

#[test]
fn an_offset_expression_shifts_each_variant_anchor() {
    // With `anchor-variants` an offset is mirrored per anchor, so the
    // expression must give the same positions the constant does.
    let constant = run(
        &recipe(r#", "anchor-variants": ["left", "right"], "offset-em": [1, 0]"#),
        centre_label(),
    );
    let expr = run(
        &recipe(r#", "anchor-variants": ["left", "right"], "offset-em-expr": ["literal", [1, 0]]"#),
        centre_label(),
    );
    assert_eq!(boxes(&constant.0), boxes(&expr.0));
    assert!(constant.1.pixels == expr.1.pixels, "the renders differ");
}

#[test]
fn a_line_label_takes_its_offset_from_the_expression() {
    let line = || {
        let mut geometry = Geometry::default();
        geometry.lines.push(vec![(256, 2048), (3840, 2048)]);
        vec![feature("aa", "x", geometry)]
    };
    let line_recipe = |extra: &str| {
        recipe_with(
            r#""glyph""#,
            &format!(r#", "placement": "line-center" {extra}"#),
        )
    };
    let constant = run(&line_recipe(r#", "offset-em": [1, 1]"#), line());
    let expr = run(
        &line_recipe(r#", "offset-em-expr": ["literal", [1, 1]]"#),
        line(),
    );
    let plain = run(&line_recipe(""), line());
    assert!(constant.1.pixels != plain.1.pixels);
    assert_eq!(boxes(&constant.0), boxes(&expr.0));
    assert!(constant.1.pixels == expr.1.pixels, "the renders differ");
}

#[test]
fn a_position_list_overrides_the_offset_expression_with_a_warning() {
    let w = warnings(&recipe(
        r#", "offset-em-expr": ["literal", [1, 0]], "anchor-offsets": [{ "anchor": "top" }]"#,
    ))
    .expect("builds");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("`offset-em-expr`"), "{}", w[0]);
    assert!(w[0].contains("ignored"), "{}", w[0]);
}

#[test]
fn a_mistyped_expression_fails_the_build() {
    for (field, expr) in [
        ("justify-expr", r#"["+", 1, 2]"#),
        ("offset-em-expr", r#"["literal", "left"]"#),
        ("max-width-em-expr", r#"["literal", "wide"]"#),
    ] {
        let err = warnings(&recipe(&format!(r#", "{field}": {expr}"#))).expect_err(field);
        assert!(err.contains(field), "{err}");
    }
}
