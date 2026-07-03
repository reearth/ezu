//! Expression-form layer filters emit as a raw `filter-expr` on the
//! `features` node (evaluated by ezu-paint via `maplibre-expr`). Legacy-form
//! filters are unsupported: the layer is left unfiltered and a warning is
//! reported.

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
fn legacy_filter_is_unsupported_and_layer_left_unfiltered() {
    let (recipe, warnings) = convert_one(json!({
        "id": "roads",
        "type": "line",
        "source": "src",
        "source-layer": "transportation",
        "filter": ["==", "class", "primary"],
        "paint": { "line-color": "#ff0000" },
    }));

    let feat = features_node(&recipe);
    // A legacy filter is unsupported: the layer carries neither a structured
    // `filter` nor a raw `filter-expr` — it is left unfiltered.
    assert!(
        feat.get("filter").is_none(),
        "legacy filter must not emit a structured `filter`: {feat}"
    );
    assert!(
        feat.get("filter-expr").is_none(),
        "legacy filter must not emit a raw `filter-expr`: {feat}"
    );
    // …and a warning explains the legacy form is unsupported.
    let joined = warnings.join("\n");
    assert!(
        joined.contains("legacy filter"),
        "legacy filter should warn about the unsupported form, got:\n{joined}"
    );
}

#[test]
fn data_driven_fill_with_expression_filter_carries_both() {
    // A data-driven `fill-color` `match` becomes a single `fill-solid` with a
    // `fill-expr`; the layer's own expression filter still rides along on the
    // `features` node as `filter-expr`. Both survive together (feature
    // selection via the filter, per-feature color via the expression).
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
    // The single features node carries the layer's expression `filter-expr`.
    let feat = features_node(&recipe);
    assert_eq!(
        feat.get("filter-expr"),
        Some(&json!(["all", ["==", ["get", "class"], "park"]])),
        "features node should carry the layer's expression filter: {feat}"
    );
    // The fill-solid carries the data-driven color as `fill-expr` (referencing
    // the driving property `kind`), not expanded into membership buckets.
    let fill = nodes
        .values()
        .find(|n| n["op"] == "fill-solid")
        .expect("a fill-solid node");
    let fill_expr = fill
        .get("fill-expr")
        .expect("fill-solid should carry a fill-expr");
    assert!(
        serde_json::to_string(fill_expr).unwrap().contains("kind"),
        "fill-expr should reference the driving property `kind`: {fill}"
    );
}
