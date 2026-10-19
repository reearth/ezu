//! `text` / `text-labels` `dedupe-key-expr`: of several labels with the same
//! key, one that has placed hides the others within `dedupe-distance-px`.
//! Read through `label-boxes` with `include: all`, which reports the hidden
//! ones with `drop-reason: dedupe`.

use crate::common::features_of_node;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::{build_graph, TileId};
use ezu_paint::nodes::{default_registry, FilteredFeatures};
use ezu_style::Document;
use std::collections::HashMap;

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

fn layer(features: Vec<Feature>) -> FeatureLayer {
    FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features,
    }
}

/// Labels named by `name` through `text-labels` → `label-placement`, with
/// every candidate's collision box from `label-boxes`. `extra` adds fields
/// to the label layer.
fn recipe(extra: &str) -> String {
    format!(
        r##"{{
      "name": "dedupe",
      "tile-size": {TILE},
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats":  {{ "op": "features", "source": "src", "layer": "pts" }},
        "labels": {{ "op": "text-labels", "features": "@feats", "source": "src", "layer": "pts",
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

const DEDUPE: &str = r#", "dedupe-key-expr": ["get", "name"], "dedupe-distance-px": 50"#;

fn run(extra: &str, tile: TileId, bindings: &[(&str, FeatureLayer)]) -> Vec<Label> {
    labels(&features_of_node(&recipe(extra), TILE, 0, tile, bindings, "boxes").0)
}

fn warnings(extra: &str) -> Result<Vec<String>, String> {
    let doc = Document::from_json(&recipe(extra)).map_err(|e| e.to_string())?;
    build_graph(&doc, &default_registry())
        .map(|g| g.warnings().to_vec())
        .map_err(|e| e.to_string())
}

/// One emitted collision box: its label, whether it placed, why not, and
/// its centre in px.
#[derive(Debug, Clone, PartialEq)]
struct Label {
    id: f64,
    placed: bool,
    reason: Option<String>,
    centre: (f32, f32),
}

fn labels(feats: &FilteredFeatures) -> Vec<Label> {
    let unit = 4096.0 / TILE as f32;
    let mut v: Vec<Label> = feats
        .groups
        .iter()
        .map(|g| {
            let ring = &g.polygons[0].exterior;
            let n = ring.len() as f32;
            let (sx, sy) = ring.iter().fold((0.0, 0.0), |(sx, sy), &(x, y)| {
                (sx + x as f32, sy + y as f32)
            });
            Label {
                id: match g.properties.get("label-id") {
                    Some(maplibre_expr::Value::Number(n)) => *n,
                    other => panic!("label-id: {other:?}"),
                },
                placed: match g.properties.get("placed") {
                    Some(maplibre_expr::Value::Bool(b)) => *b,
                    other => panic!("placed: {other:?}"),
                },
                reason: match g.properties.get("drop-reason") {
                    Some(maplibre_expr::Value::String(s)) => Some(s.clone()),
                    None => None,
                    other => panic!("drop-reason: {other:?}"),
                },
                centre: (sx / n / unit, sy / n / unit),
            }
        })
        .collect();
    v.sort_by(|a, b| a.id.total_cmp(&b.id));
    v
}

/// Three buildings named alike: two 32 px apart, one across the tile. None
/// of their boxes overlap, so collision alone would show all three.
fn three_halls() -> Vec<Feature> {
    vec![
        point("Hall", 1024, 1024),
        point("Hall", 1024, 2048),
        point("Hall", 3584, 3584),
    ]
}

fn centre_tile() -> TileId {
    TileId { z: 0, x: 0, y: 0 }
}

#[test]
fn a_nearby_label_with_the_same_key_is_dropped_and_reported() {
    let plain = run("", centre_tile(), &[("src.pts", layer(three_halls()))]);
    assert_eq!(plain.iter().filter(|l| l.placed).count(), 3, "{plain:?}");

    let got = run(DEDUPE, centre_tile(), &[("src.pts", layer(three_halls()))]);
    let placed: Vec<_> = got.iter().filter(|l| l.placed).collect();
    let dropped: Vec<_> = got.iter().filter(|l| !l.placed).collect();
    assert_eq!((placed.len(), dropped.len()), (2, 1), "{got:?}");
    assert_eq!(dropped[0].reason.as_deref(), Some("dedupe"));
    // The first in feature order shows; its neighbour 32 px below is the one
    // dropped, and the far one shows.
    let (x, y) = dropped[0].centre;
    assert!((x - 32.0).abs() < 1.0 && (y - 64.0).abs() < 2.0, "{got:?}");
}

#[test]
fn different_names_are_not_duplicates() {
    let feats = vec![point("Hall", 1024, 1024), point("Annex", 1024, 2048)];
    let got = run(DEDUPE, centre_tile(), &[("src.pts", layer(feats))]);
    assert!(got.len() == 2 && got.iter().all(|l| l.placed), "{got:?}");
}

#[test]
fn the_same_label_is_dropped_on_both_sides_of_a_seam() {
    // One `Hall` near the right edge of x=5, one just past it in x=6, about
    // 42 px apart and clear of each other's boxes. Each tile sees the other's
    // as a neighbour, and both must keep the same one.
    let left = || layer(vec![point("Hall", 3800, 2048)]);
    let right = || layer(vec![point("Hall", 300, 3248)]);
    let on_left = run(
        DEDUPE,
        TileId { z: 4, x: 5, y: 7 },
        &[("src.pts", left()), ("src.pts@1,0", right())],
    );
    let on_right = run(
        DEDUPE,
        TileId { z: 4, x: 6, y: 7 },
        &[("src.pts@-1,0", left()), ("src.pts", right())],
    );
    let decisions = |v: &[Label]| -> Vec<(f64, bool, Option<String>)> {
        v.iter()
            .map(|l| (l.id, l.placed, l.reason.clone()))
            .collect()
    };
    assert_eq!(on_left.len(), 2, "{on_left:?}");
    assert_eq!(decisions(&on_left), decisions(&on_right));
    let dropped: Vec<_> = on_left.iter().filter(|l| !l.placed).collect();
    assert_eq!(dropped.len(), 1, "{on_left:?}");
    assert_eq!(dropped[0].reason.as_deref(), Some("dedupe"));
    // Without dedupe both show, so it is the dedupe that drops one.
    let plain = run(
        "",
        TileId { z: 4, x: 5, y: 7 },
        &[("src.pts", left()), ("src.pts@1,0", right())],
    );
    assert!(
        plain.len() == 2 && plain.iter().all(|l| l.placed),
        "{plain:?}"
    );
}

#[test]
fn the_dedupe_distance_is_required_and_positive() {
    let err = warnings(r#", "dedupe-key-expr": ["get", "name"]"#).expect_err("no distance");
    assert!(
        err.contains("dedupe-distance-px") && err.contains("required"),
        "{err}"
    );
    for px in ["0", "-5"] {
        let err = warnings(&format!(
            r#", "dedupe-key-expr": ["get", "name"], "dedupe-distance-px": {px}"#
        ))
        .expect_err("a non-positive distance");
        assert!(
            err.contains("dedupe-distance-px") && err.contains("greater than 0"),
            "{err}"
        );
    }
    assert!(warnings(DEDUPE).expect("builds").is_empty());
}

#[test]
fn line_placement_ignores_the_dedupe_with_a_warning() {
    let w = warnings(&format!(r#"{DEDUPE}, "placement": "line""#)).expect("builds");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].contains("`dedupe-key-expr` applies to point placement only"),
        "{}",
        w[0]
    );
}
