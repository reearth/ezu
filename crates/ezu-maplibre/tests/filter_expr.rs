//! Expression-form layer filters emit as a raw `filter-expr` on the
//! `features` node (evaluated by ezu-paint via `maplibre-expr`), while
//! legacy-form filters keep the structured `filter` translation.

use ezu_maplibre::{convert, ConvertOptions};
use serde_json::{json, Value};

fn convert_one(layer: Value) -> (Value, Vec<String>) {
    let style = json!({
        "version": 8,
        "sources": {
            "src": { "type": "vector", "tiles": ["https://example.com/{z}/{x}/{y}.pbf"] }
        },
        "layers": [layer],
    });
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("conversion");
    // Every recipe must parse as a real ezu Document.
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
    (recipe, report.warnings)
}

/// The one `features` node in a single-layer recipe.
fn features_node(recipe: &Value) -> &Value {
    recipe["nodes"]
        .as_object()
        .unwrap()
        .values()
        .find(|n| n["op"] == "features")
        .expect("a features node")
}

#[test]
fn expression_filter_emits_filter_expr_no_structured_filter() {
    let expr = json!(["all", ["==", ["get", "class"], "primary"], ["has", "name"]]);
    let (recipe, warnings) = convert_one(json!({
        "id": "roads",
        "type": "line",
        "source": "src",
        "source-layer": "transportation",
        "filter": expr,
        "paint": { "line-color": "#ff0000" },
    }));

    let feat = features_node(&recipe);
    // Raw expression passed through verbatim.
    assert_eq!(
        feat.get("filter-expr"),
        Some(&json!([
            "all",
            ["==", ["get", "class"], "primary"],
            ["has", "name"]
        ])),
    );
    // No structured `filter` — the expression is not translated.
    assert!(
        feat.get("filter").is_none(),
        "expression filter must not also emit a structured `filter`: {feat}"
    );

    // No warning about this layer's filter (full fidelity, nothing dropped).
    let joined = warnings.join("\n");
    assert!(
        !joined.contains("filter") && !joined.contains("unsupported"),
        "expression filter should convert without warning, got:\n{joined}"
    );
}

#[test]
fn legacy_filter_emits_structured_filter_no_filter_expr() {
    let (recipe, _warnings) = convert_one(json!({
        "id": "roads",
        "type": "line",
        "source": "src",
        "source-layer": "transportation",
        "filter": ["==", "class", "primary"],
        "paint": { "line-color": "#ff0000" },
    }));

    let feat = features_node(&recipe);
    // Legacy filter → structured `filter` object.
    let structured = feat.get("filter").and_then(|f| f.as_object());
    assert!(
        structured.is_some(),
        "legacy filter should emit a structured `filter` object: {feat}"
    );
    assert_eq!(structured.unwrap().get("class"), Some(&json!("primary")));
    // …and NOT a raw `filter-expr`.
    assert!(
        feat.get("filter-expr").is_none(),
        "legacy filter must not emit a raw `filter-expr`: {feat}"
    );
}

#[test]
fn bucket_fill_with_expression_filter_carries_both() {
    // A `fill-color` `match` expands into per-bucket membership `filter`s;
    // the layer's own expression filter still rides along as `filter-expr`.
    // ezu-paint ANDs the two.
    let (recipe, _warnings) = convert_one(json!({
        "id": "areas",
        "type": "fill",
        "source": "src",
        "source-layer": "landuse",
        "filter": ["all", ["==", ["get", "class"], "park"]],
        "paint": {
            "fill-color": [
                "match", ["get", "kind"],
                "forest", "#00ff00",
                "grass", "#88ff88",
                "#cccccc"
            ]
        },
    }));

    let nodes = recipe["nodes"].as_object().unwrap();
    // A bucket features node carries BOTH the membership `filter` (structured)
    // and the layer's expression `filter-expr`.
    let bucket = nodes
        .values()
        .find(|n| n["op"] == "features" && n.get("filter").and_then(|f| f.get("kind")).is_some());
    let bucket = bucket.expect("a bucket features node with a `kind` membership filter");
    assert_eq!(
        bucket.get("filter-expr"),
        Some(&json!(["all", ["==", ["get", "class"], "park"]])),
        "bucket node should also carry the layer's expression filter: {bucket}"
    );
}
