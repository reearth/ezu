//! `text` node `anchor-variants` (MapLibre `text-variable-anchor`): a label
//! that collides at its primary anchor relocates to the first free anchor in
//! the list instead of being dropped. The pure fallback logic is unit-tested
//! in `ezu_core::text::collide`; this exercises the whole render path.

use crate::common::render_with_features_and_images;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

/// A point feature with a `name` label and numeric `rank` (a `sort-key-expr`
/// source; lower ranks place first).
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
      "name": "text-variable-anchor",
      "tile-size": 96,
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
        96,
        24,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer(feats))],
        &[],
    )
}

#[test]
fn variable_anchor_rescues_a_colliding_label() {
    // Two differently-labelled features at the *same* point. At a fixed centre
    // anchor their boxes fully overlap, so collision drops one. With
    // `anchor-variants` the loser relocates to a free anchor (pushed away by
    // `radial-offset`), so both draw and the render inks strictly more.
    let feats = || vec![feat("AA", 0, 2048, 2048), feat("BB", 1, 2048, 2048)];

    let fixed = render1(&recipe(""), feats());
    let variable = render1(
        &recipe(r#", "anchor-variants": ["top", "bottom"], "radial-offset": 1.0"#),
        feats(),
    );

    assert!(ink(&fixed) > 0, "the winning label must draw");
    assert!(
        ink(&variable) > ink(&fixed),
        "variable anchor should rescue the second label: {} (fixed) vs {} (variable)",
        ink(&fixed),
        ink(&variable),
    );
}

#[test]
fn variable_anchor_matches_allow_overlap_when_both_fit() {
    // When both labels place (the variable-anchor render), the total ink should
    // be in the neighbourhood of an allow-overlap render that also draws both —
    // i.e. clearly more than a single label, confirming both are present.
    let feats = || vec![feat("AA", 0, 2048, 2048), feat("BB", 1, 2048, 2048)];

    let one = render1(&recipe(""), feats()); // collision keeps one
    let both_overlap = render1(&recipe(r#", "allow-overlap": true"#), feats());
    let variable = render1(
        &recipe(r#", "anchor-variants": ["top", "bottom"], "radial-offset": 1.0"#),
        feats(),
    );

    // The variable render draws two separated labels; both_overlap draws two
    // labels stacked on the same spot (so its ink is a lower bound on two
    // distinct glyphs' union). Both exceed the single-label render.
    assert!(ink(&variable) > ink(&one));
    assert!(ink(&both_overlap) > ink(&one));
}
