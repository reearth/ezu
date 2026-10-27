//! `text` / `text-labels` `overlap`: `never` drops a label with no free
//! position, `always` shows it at its first without trying the rest, and
//! `fallback` tries every position first and only then shows it at the first,
//! overlapping. Read through `label-boxes`, whose `variant-index` and
//! `overlapped` say where each label went.

use crate::common::features_of_node;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::{build_graph, RasterBuf, TileId};
use ezu_paint::nodes::{default_registry, FilteredFeatures};
use ezu_style::Document;
use std::collections::HashMap;
use std::sync::Arc;

const TILE: u32 = 128;

fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

fn point(name: &str, x: i32, y: i32) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), Value::String(name.to_string()));
    let mut geometry = Geometry::default();
    geometry.points.push((x, y));
    Feature {
        id: None,
        geometry,
        properties,
    }
}

/// Two labels on one point, through `text-labels` → `label-placement`, drawn
/// by `text-draw` over their `label-boxes`. `extra` adds fields to the label
/// layer.
fn recipe(extra: &str) -> String {
    format!(
        r##"{{
      "name": "overlap",
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
        "boxes":  {{ "op": "label-boxes", "placement": "@placed",
                     "parts": ["collision"], "include": "all" }},
        "fill":   {{ "op": "fill-solid", "features": "@boxes", "fill": "#0000ff" }},
        "text":   {{ "op": "text-draw", "labels": "@labels", "placement": "@placed" }},
        "out":    {{ "op": "stack", "layers": ["@fill", "@text"] }}
      }},
      "output": "@out"
    }}"##,
        font = font_url(),
    )
}

fn run(extra: &str) -> (Arc<FilteredFeatures>, Arc<RasterBuf>) {
    let layer = FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features: vec![point("AA", 2048, 2048), point("BB", 2048, 2048)],
    };
    features_of_node(
        &recipe(extra),
        TILE,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer)],
        "boxes",
    )
}

fn warnings(extra: &str) -> Result<Vec<String>, String> {
    let doc = Document::from_json(&recipe(extra)).map_err(|e| e.to_string())?;
    build_graph(&doc, &default_registry())
        .map(|g| g.warnings().to_vec())
        .map_err(|e| e.to_string())
}

/// Each label's `(variant-index, overlapped)`, `None`s for one that dropped,
/// sorted.
fn outcomes(feats: &FilteredFeatures) -> Vec<(Option<f64>, Option<bool>)> {
    let mut v: Vec<_> = feats
        .groups
        .iter()
        .map(|g| {
            let ix = match g.properties.get("variant-index") {
                Some(maplibre_expr::Value::Number(n)) => Some(*n),
                None => None,
                other => panic!("variant-index: {other:?}"),
            };
            let overlapped = match g.properties.get("overlapped") {
                Some(maplibre_expr::Value::Bool(b)) => Some(*b),
                None => None,
                other => panic!("overlapped: {other:?}"),
            };
            (ix, overlapped)
        })
        .collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v
}

/// A centred position, then one far enough below the point to clear it.
const TWO_POSITIONS: &str = r#", "anchor-offsets": [
    { "anchor": "center" },
    { "anchor": "top", "radial-em": 2 }
]"#;

#[test]
fn fallback_moves_to_a_free_position_when_there_is_one() {
    let got = outcomes(&run(&format!(r#"{TWO_POSITIONS}, "overlap": "fallback""#)).0);
    assert_eq!(
        got,
        [(Some(0.0), Some(false)), (Some(1.0), Some(false))],
        "the second label moves, overlapping nothing"
    );
}

#[test]
fn fallback_overlaps_at_the_first_position_when_every_one_is_blocked() {
    // One position only: the second label has nowhere to move, so it shows
    // where the first did, marked overlapped.
    let got = outcomes(&run(r#", "anchor": "center", "overlap": "fallback""#).0);
    assert_eq!(got, [(Some(0.0), Some(false)), (Some(0.0), Some(true))]);
    // `never` drops it instead.
    let got = outcomes(&run(r#", "anchor": "center""#).0);
    assert_eq!(got, [(None, None), (Some(0.0), Some(false))]);
}

#[test]
fn always_never_tries_a_later_position() {
    let got = outcomes(&run(&format!(r#"{TWO_POSITIONS}, "overlap": "always""#)).0);
    assert_eq!(got, [(Some(0.0), Some(false)), (Some(0.0), Some(true))]);
}

#[test]
fn allow_overlap_still_means_always() {
    let (old_boxes, old) = run(&format!(r#"{TWO_POSITIONS}, "allow-overlap": true"#));
    let (new_boxes, new) = run(&format!(r#"{TWO_POSITIONS}, "overlap": "always""#));
    assert_eq!(outcomes(&old_boxes), outcomes(&new_boxes));
    assert!(old.pixels == new.pixels, "the renders differ");
}

#[test]
fn allow_overlap_warns_and_names_its_replacement() {
    let w = warnings(r#", "allow-overlap": true"#).expect("builds");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].contains("`allow-overlap` is deprecated") && w[0].contains(r#""overlap": "always""#),
        "{}",
        w[0]
    );
    let w = warnings(r#", "allow-overlap": false"#).expect("builds");
    assert!(w[0].contains(r#""overlap": "never""#), "{}", w[0]);
    // Both set: `overlap` wins and the old field is named as ignored.
    let w = warnings(r#", "allow-overlap": true, "overlap": "fallback""#).expect("builds");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("`allow-overlap` ignored"), "{}", w[0]);
    assert!(warnings(r#", "overlap": "always""#)
        .expect("builds")
        .is_empty());
    let err = warnings(r#", "overlap": "sometimes""#).expect_err("an unknown mode");
    assert!(err.contains("unknown overlap `sometimes`"), "{err}");
}
