//! Data-driven paint conversion: a `match`/`interpolate` fill or line paint
//! property is emitted as a single node carrying a MapLibre expression
//! (`fill-expr` / `color-expr` / `width-expr`), not expanded into N filtered
//! buckets.

use ezu_translate::maplibre::{convert, ConvertOptions};
use serde_json::json;

/// A minimal style with a single vector source and one layer.
fn style_with_layer(layer: serde_json::Value) -> serde_json::Value {
    json!({
        "version": 8,
        "sources": {
            "src": { "type": "vector", "url": "http://example.invalid/tiles.json" }
        },
        "layers": [layer]
    })
}

#[test]
fn data_driven_fill_color_emits_one_fill_expr_node() {
    let style = style_with_layer(json!({
        "id": "polys",
        "type": "fill",
        "source": "src",
        "source-layer": "land",
        "paint": {
            "fill-color": ["match", ["get", "class"], "a", "#ff0000", "#00ff00"]
        }
    }));
    let (recipe, _report) = convert(&style, &ConvertOptions::default()).expect("conversion");

    let nodes = recipe["nodes"].as_object().unwrap();
    // Exactly one fill-solid, carrying a fill-expr (no N-bucket expansion).
    let fills: Vec<_> = nodes.values().filter(|n| n["op"] == "fill-solid").collect();
    assert_eq!(
        fills.len(),
        1,
        "expected exactly one fill-solid, got {}",
        fills.len()
    );
    let fill = fills[0];
    assert!(
        fill.get("fill-expr").map(|e| e.is_array()).unwrap_or(false),
        "fill-solid should carry a data-driven fill-expr: {fill}"
    );
    // The raw expression references the driving property.
    let serialized = serde_json::to_string(&fill["fill-expr"]).unwrap();
    assert!(
        serialized.contains("class"),
        "fill-expr should reference `class`: {serialized}"
    );

    // The recipe still parses as a real ezu Document.
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}

#[test]
fn data_driven_fill_opacity_emits_opacity_expr() {
    // A feature-driven `fill-opacity` must become an `opacity-expr` on
    // `fill-solid`, while a constant/zoom `fill-color` still bakes.
    let style = style_with_layer(json!({
        "id": "polys",
        "type": "fill",
        "source": "src",
        "source-layer": "land",
        "paint": {
            "fill-color": "#3388ff",
            "fill-opacity": ["interpolate", ["linear"], ["get", "score"], 0, 0.1, 1, 0.9]
        }
    }));
    let (recipe, _report) = convert(&style, &ConvertOptions::default()).expect("conversion");

    let nodes = recipe["nodes"].as_object().unwrap();
    let fills: Vec<_> = nodes.values().filter(|n| n["op"] == "fill-solid").collect();
    assert_eq!(fills.len(), 1, "expected exactly one fill-solid");
    let fill = fills[0];

    assert!(
        fill.get("opacity-expr")
            .map(|e| e.is_array())
            .unwrap_or(false),
        "fill-solid should carry a data-driven opacity-expr: {fill}"
    );
    let serialized = serde_json::to_string(&fill["opacity-expr"]).unwrap();
    assert!(
        serialized.contains("score"),
        "opacity-expr should reference `score`: {serialized}"
    );
    // Data-driven opacity → no baked `fill-alpha`; color still bakes.
    assert!(fill.get("fill-alpha").is_none());
    assert_eq!(fill["fill"], "#3388ff");

    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}

#[test]
fn data_driven_line_emits_color_expr_and_raw_width_expr() {
    let width_fn = json!(["interpolate", ["linear"], ["zoom"], 10, 1, 16, 4]);
    let style = style_with_layer(json!({
        "id": "roads",
        "type": "line",
        "source": "src",
        "source-layer": "transportation",
        "paint": {
            "line-color": ["match", ["get", "class"], "a", "#ff0000", "#00ff00"],
            "line-width": width_fn
        }
    }));
    // Recipes are zoom-independent: the zoom `interpolate` on `line-width` is
    // emitted raw as `width-expr` (evaluated per tile), NOT baked to a
    // constant; the colour `match` is data-driven → a `color-expr`.
    let (recipe, _report) = convert(&style, &ConvertOptions::default()).expect("conversion");

    let nodes = recipe["nodes"].as_object().unwrap();
    let strokes: Vec<_> = nodes.values().filter(|n| n["op"] == "stroke").collect();
    assert_eq!(strokes.len(), 1, "expected exactly one stroke node");
    let stroke = strokes[0];

    // Data-driven color → color-expr referencing `class`.
    assert!(
        stroke
            .get("color-expr")
            .map(|e| e.is_array())
            .unwrap_or(false),
        "stroke should carry a data-driven color-expr: {stroke}"
    );
    let serialized = serde_json::to_string(&stroke["color-expr"]).unwrap();
    assert!(
        serialized.contains("class"),
        "color-expr should reference `class`: {serialized}"
    );

    // Zoom width is emitted raw as `width-expr` (not baked).
    assert_eq!(
        stroke.get("width-expr"),
        Some(&width_fn),
        "zoom width should be emitted raw as width-expr: {stroke}"
    );

    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}

#[test]
fn zoom_functions_are_emitted_raw_not_baked() {
    // Legacy `{stops}` on `line-width` → `stroke.width-expr` holds the object
    // verbatim (not baked to a constant).
    let stops = json!({ "stops": [[10, 1], [16, 4]] });
    let style = style_with_layer(json!({
        "id": "roads",
        "type": "line",
        "source": "src",
        "source-layer": "transportation",
        "paint": { "line-width": stops }
    }));
    let (recipe, _) = convert(&style, &ConvertOptions::default()).expect("conversion");
    let nodes = recipe["nodes"].as_object().unwrap();
    let stroke = nodes
        .values()
        .find(|n| n["op"] == "stroke")
        .expect("a stroke node");
    assert_eq!(
        stroke.get("width-expr"),
        Some(&stops),
        "legacy {{stops}} width should be emitted raw: {stroke}"
    );

    // `interpolate` on zoom for `fill-color` → `fill-expr` holds it verbatim.
    let fill_fn = json!([
        "interpolate",
        ["linear"],
        ["zoom"],
        10,
        "#000000",
        16,
        "#ffffff"
    ]);
    let style = style_with_layer(json!({
        "id": "polys",
        "type": "fill",
        "source": "src",
        "source-layer": "land",
        "paint": { "fill-color": fill_fn }
    }));
    let (recipe, _) = convert(&style, &ConvertOptions::default()).expect("conversion");
    let nodes = recipe["nodes"].as_object().unwrap();
    let fill = nodes
        .values()
        .find(|n| n["op"] == "fill-solid")
        .expect("a fill-solid node");
    assert_eq!(
        fill.get("fill-expr"),
        Some(&fill_fn),
        "zoom fill-color should be emitted raw: {fill}"
    );

    // A layer's `minzoom`/`maxzoom` → the features node's gate.
    let style = style_with_layer(json!({
        "id": "polys",
        "type": "fill",
        "source": "src",
        "source-layer": "land",
        "minzoom": 5,
        "maxzoom": 12,
        "paint": { "fill-color": "#123456" }
    }));
    let (recipe, _) = convert(&style, &ConvertOptions::default()).expect("conversion");
    let nodes = recipe["nodes"].as_object().unwrap();
    let feat = nodes
        .values()
        .find(|n| n["op"] == "features")
        .expect("a features node");
    assert_eq!(feat["min-zoom"], 5, "features node should gate min-zoom");
    // `maxzoom` is exclusive in MapLibre, `max-zoom` inclusive in ezu, so
    // the gate sits a level lower: this layer draws z5 through z11.
    assert_eq!(feat["max-zoom"], 11, "features node should gate max-zoom");

    // And the recipe still builds.
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}
