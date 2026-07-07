//! `text` node `padding-expr` (data-driven MapLibre `text-padding`): the
//! collision-box inflation is evaluated per feature, so a zoom-varying padding
//! tightens or loosens label spacing by zoom.

mod common;
use common::render_with_features_and_images;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

fn feat(name: &str, rank: i64, x: i32, y: i32) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), Value::String(name.to_string()));
    properties.insert("rank".to_string(), Value::Int(rank));
    let mut geometry = Geometry::default();
    geometry.points.push((x, y));
    Feature {
        id: None,
        geometry,
        properties,
    }
}

fn layer(features: Vec<Feature>) -> FeatureLayer {
    FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features,
    }
}

fn recipe(extra: &str) -> String {
    format!(
        r##"{{
      "name": "text-padding-expr",
      "tile-size": 64,
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        "out":   {{ "op": "text", "features": "@feats", "font": ["body"],
                    "text": ["get", "name"], "size": 24,
                    "sort-key-expr": ["get", "rank"] {extra} }}
      }},
      "output": "@out"
    }}"##,
        font = font_url(),
        extra = extra,
    )
}

fn ink(r: &ezu_graph::RasterBuf) -> usize {
    let mut n = 0;
    for y in 0..r.height {
        for x in 0..r.width {
            if r.pixel(x, y)[3] > 100 {
                n += 1;
            }
        }
    }
    n
}

fn render1(recipe: &str, feats: Vec<Feature>) -> std::sync::Arc<ezu_graph::RasterBuf> {
    render_with_features_and_images(
        recipe,
        64,
        8,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer(feats))],
        &[],
    )
}

#[test]
fn padding_expr_overrides_constant_padding() {
    // Two narrow labels with a clear gap. A tiny `padding-expr` leaves them
    // both placed; a huge `padding-expr` inflates their boxes into a collision
    // that drops one — matching what a constant `padding-px` would do, but
    // driven by the expression.
    let feats = || vec![feat("II", 0, 1200, 2048), feat("LL", 1, 2900, 2048)];
    let small = render1(&recipe(r#", "padding-expr": ["literal", 1]"#), feats());
    let big = render1(&recipe(r#", "padding-expr": ["literal", 400]"#), feats());
    assert!(ink(&small) > 0, "both labels should draw at small padding");
    assert!(
        ink(&big) < ink(&small),
        "a huge padding-expr should force a collision the small one avoids: {} vs {}",
        ink(&big),
        ink(&small),
    );
}

#[test]
fn padding_expr_falls_back_to_constant_when_absent() {
    // With no `padding-expr`, the constant `padding-px` still governs: a huge
    // constant padding drops one of the same two labels.
    let feats = || vec![feat("II", 0, 1200, 2048), feat("LL", 1, 2900, 2048)];
    let small = render1(&recipe(r#", "padding-px": 1"#), feats());
    let big = render1(&recipe(r#", "padding-px": 400"#), feats());
    assert!(ink(&big) < ink(&small));
}

#[test]
fn zoom_curve_padding_matches_its_evaluated_constant() {
    // A zoom-interpolated `padding-expr` evaluated at z0 must render exactly as
    // the constant it resolves to there — here the curve is flat at 400 near
    // z0, so it drops the loser just like `padding-px: 400`.
    let feats = || vec![feat("II", 0, 1200, 2048), feat("LL", 1, 2900, 2048)];
    let curve = render1(
        &recipe(r#", "padding-expr": ["interpolate", ["linear"], ["zoom"], 0, 400, 22, 400]"#),
        feats(),
    );
    let constant = render1(&recipe(r#", "padding-px": 400"#), feats());
    assert_eq!(
        curve.pixels, constant.pixels,
        "a flat zoom curve must match its constant padding"
    );
}
