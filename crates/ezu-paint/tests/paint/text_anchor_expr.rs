//! `text` / `text-labels` `anchor-expr`: the anchor of a point label's single
//! position, chosen per feature group by a MapLibre expression. Read through
//! `label-boxes`, whose text boxes show which side of its point each label
//! landed on.

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

/// A point labelled `name`, carrying a `side` property for the expression.
fn point(name: &str, side: &str, x: i32, y: i32) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), Value::String(name.to_string()));
    properties.insert("side".to_string(), Value::String(side.to_string()));
    let mut geometry = Geometry::default();
    geometry.points.push((x, y));
    Feature {
        id: None,
        geometry,
        properties,
    }
}

/// 16 px labels through `text-labels` → `label-placement`, drawn by
/// `text-draw` over their `label-boxes`. `extra` adds fields to the label
/// layer.
fn recipe(extra: &str) -> String {
    format!(
        r##"{{
      "name": "anchor-expr",
      "tile-size": {TILE},
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats":  {{ "op": "features", "source": "src", "layer": "pts" }},
        "fonts":  {{ "op": "fonts", "font-stacks": {{ "body": ["body"] }} }},
        "labels": {{ "op": "text-labels", "features": "@feats", "fonts": "@fonts",
                     "font": ["body"], "text": ["get", "name"], "size": 16,
                     "color": "#ffffff" {extra} }},
        "placed": {{ "op": "label-placement", "labels": ["@labels"] }},
        "boxes":  {{ "op": "label-boxes", "placement": "@placed", "parts": ["text"] }},
        "fill":   {{ "op": "fill-solid", "features": "@boxes", "fill": "#0000ff" }},
        "text":   {{ "op": "text-draw", "labels": "@labels", "placement": "@placed" }},
        "out":    {{ "op": "stack", "layers": ["@fill", "@text"] }}
      }},
      "output": "@out"
    }}"##,
        font = font_url(),
    )
}

fn run_at(recipe: &str, z: u8, features: Vec<Feature>) -> (Arc<FilteredFeatures>, Arc<RasterBuf>) {
    let layer = FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features,
    };
    features_of_node(
        recipe,
        TILE,
        0,
        TileId { z, x: 0, y: 0 },
        &[("src.pts", layer)],
        "boxes",
    )
}

fn run(recipe: &str, features: Vec<Feature>) -> (Arc<FilteredFeatures>, Arc<RasterBuf>) {
    run_at(recipe, 0, features)
}

/// The build-time warnings of a style, or its build error.
fn warnings(recipe: &str) -> Result<Vec<String>, String> {
    let doc = Document::from_json(recipe).map_err(|e| e.to_string())?;
    build_graph(&doc, &default_registry())
        .map(|g| g.warnings().to_vec())
        .map_err(|e| e.to_string())
}

/// Each placed text box `(min_x, min_y, max_x, max_y)` in px, left to right.
fn text_boxes(feats: &FilteredFeatures) -> Vec<(f32, f32, f32, f32)> {
    let mut v: Vec<_> = feats
        .groups
        .iter()
        .flat_map(|g| {
            g.polygons.iter().map(|p| {
                p.exterior.iter().fold(
                    (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
                    |(x0, y0, x1, y1), &(x, y)| {
                        let (x, y) = (x as f32 / UNITS_PER_PX, y as f32 / UNITS_PER_PX);
                        (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
                    },
                )
            })
        })
        .collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v
}

const BY_SIDE: &str = r#", "anchor-expr": ["match", ["get", "side"], "east", "left", "right"]"#;

#[test]
fn a_property_puts_same_text_labels_on_opposite_sides() {
    // Both labels say "AA", so only the anchor tells their layouts apart:
    // `left` starts the text at its point (x = 32 px) and grows rightward,
    // `right` ends it at its point (x = 96 px).
    let feats = vec![
        point("AA", "east", 1024, 2048),
        point("AA", "west", 3072, 2048),
    ];
    let (boxes, _) = run(&recipe(BY_SIDE), feats);
    let got = text_boxes(&boxes);
    assert_eq!(got.len(), 2, "{got:?}");
    let (east, west) = (got[0], got[1]);
    assert!(east.0 >= 32.0 - 0.5 && east.0 < 40.0, "east: {got:?}");
    assert!(east.2 > 40.0, "east grows rightward: {got:?}");
    assert!(west.2 <= 96.0 + 0.5 && west.2 > 88.0, "west: {got:?}");
    assert!(west.0 < 88.0, "west grows leftward: {got:?}");
    assert!(warnings(&recipe(BY_SIDE)).expect("builds").is_empty());
}

#[test]
fn an_expression_anchor_places_like_the_constant_it_names() {
    // A group whose expression names `right` renders exactly as the constant
    // `anchor: right` with the same `offset-em`.
    let feats = || vec![point("AA", "west", 2048, 2048)];
    let constant = run(
        &recipe(r#", "anchor": "right", "offset-em": [-0.5, 0.25]"#),
        feats(),
    );
    let expr = run(
        &recipe(&format!(r#"{BY_SIDE}, "offset-em": [-0.5, 0.25]"#)),
        feats(),
    );
    assert_eq!(text_boxes(&constant.0), text_boxes(&expr.0));
    assert!(constant.1.pixels == expr.1.pixels, "the renders differ");
}

#[test]
fn a_zoom_step_switches_the_anchor() {
    let step = r#", "anchor-expr": ["step", ["zoom"], "left", 3, "right"]"#;
    let feats = || vec![point("AA", "", 2048, 2048)];
    let low = text_boxes(&run_at(&recipe(step), 2, feats()).0);
    let high = text_boxes(&run_at(&recipe(step), 3, feats()).0);
    assert_eq!((low.len(), high.len()), (1, 1), "{low:?} {high:?}");
    // Below z3 the text starts at the point (64 px); from z3 it ends there.
    assert!(low[0].0 >= 64.0 - 0.5 && low[0].0 < 72.0, "{low:?}");
    assert!(high[0].2 <= 64.0 + 0.5 && high[0].2 > 56.0, "{high:?}");
}

#[test]
fn a_result_that_names_no_anchor_falls_back_to_anchor() {
    let feats = || vec![point("AA", "", 2048, 2048)];
    let plain = run(&recipe(r#", "anchor": "left""#), feats());
    let fallback = run(
        &recipe(r#", "anchor": "left", "anchor-expr": ["literal", "middle"]"#),
        feats(),
    );
    assert_eq!(text_boxes(&plain.0), text_boxes(&fallback.0));
    assert!(plain.1.pixels == fallback.1.pixels, "the renders differ");
}

#[test]
fn a_position_list_overrides_it_with_a_warning() {
    let w = warnings(&recipe(&format!(
        r#"{BY_SIDE}, "anchor-offsets": [{{ "anchor": "top" }}]"#
    )))
    .expect("builds");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("`anchor-expr` ignored"), "{}", w[0]);

    // The deprecated list is overridden the same way, and still placed.
    let variants = recipe(&format!(r#"{BY_SIDE}, "anchor-variants": ["top"]"#));
    let w = warnings(&variants).expect("builds");
    assert!(
        w.iter().any(|w| w.contains("`anchor-expr` ignored")),
        "{w:?}"
    );
    let alone = recipe(r#", "anchor-variants": ["top"]"#);
    let feats = || vec![point("AA", "east", 2048, 2048)];
    assert_eq!(
        text_boxes(&run(&variants, feats()).0),
        text_boxes(&run(&alone, feats()).0)
    );
}

#[test]
fn line_placement_ignores_it_with_a_warning() {
    let w = warnings(&recipe(&format!(r#"{BY_SIDE}, "placement": "line""#))).expect("builds");
    assert!(
        w.iter()
            .any(|w| w.contains("`anchor-expr` applies to point placement only")),
        "{w:?}"
    );
}

#[test]
fn a_non_string_expression_fails_the_build() {
    let err = warnings(&recipe(r#", "anchor-expr": ["+", 1, 2]"#)).expect_err("number");
    assert!(err.contains("anchor-expr"), "{err}");
}
