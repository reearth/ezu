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
    // verbatim (not baked to a constant, not guessed at) and the report
    // points at `migrate`.
    let stops = json!({ "stops": [[10, 1], [16, 4]] });
    let style = style_with_layer(json!({
        "id": "roads",
        "type": "line",
        "source": "src",
        "source-layer": "transportation",
        "paint": { "line-width": stops }
    }));
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("conversion");
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
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("`line-width` is a legacy function object")
                && w.contains("--migrate")),
        "expected a legacy-function warning: {:?}",
        report.warnings
    );

    // With `migrate`, the same style yields the expression MapLibre itself
    // would read: `line-width` interpolates, so `interpolate` on zoom.
    let opts = ConvertOptions {
        migrate: true,
        ..Default::default()
    };
    let (recipe, report) = convert(&style, &opts).expect("conversion");
    let nodes = recipe["nodes"].as_object().unwrap();
    let stroke = nodes
        .values()
        .find(|n| n["op"] == "stroke")
        .expect("a stroke node");
    assert_eq!(
        stroke.get("width-expr"),
        Some(&json!(["interpolate", ["linear"], ["zoom"], 10, 1, 16, 4])),
        "migrated {{stops}} width should be an interpolate: {stroke}"
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

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

#[test]
fn data_driven_fill_outline_color_emits_edge_expr() {
    // An expression `fill-outline-color` becomes `edge-expr` with the 1 px
    // outline a constant one gets; it used to be dropped without a warning.
    let outline = json!(["match", ["get", "class"], "park", "#336633", "#333333"]);
    let style = style_with_layer(json!({
        "id": "polys",
        "type": "fill",
        "source": "src",
        "source-layer": "land",
        "paint": { "fill-color": "#eeeeee", "fill-outline-color": outline }
    }));
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("conversion");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let nodes = recipe["nodes"].as_object().unwrap();
    let fill = nodes
        .values()
        .find(|n| n["op"] == "fill-solid")
        .expect("a fill-solid");
    assert_eq!(fill["edge-expr"], outline);
    assert_eq!(fill["edge-width"], 1.0);
    assert!(fill.get("edge").is_none());

    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}

#[test]
fn an_unreadable_fill_outline_color_warns() {
    let style = style_with_layer(json!({
        "id": "polys",
        "type": "fill",
        "source": "src",
        "source-layer": "land",
        "paint": { "fill-color": "#eeeeee", "fill-outline-color": "not-a-colour" }
    }));
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("conversion");
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("fill-outline-color")),
        "{:?}",
        report.warnings
    );
    let nodes = recipe["nodes"].as_object().unwrap();
    let fill = nodes.values().find(|n| n["op"] == "fill-solid").unwrap();
    assert!(fill.get("edge").is_none() && fill.get("edge-expr").is_none());
}

#[test]
fn fill_opacity_fades_the_outline_too() {
    // MapLibre's `fill-opacity` fades `fill-outline-color` along with the
    // fill; the node keeps the two apart, so the opacity is written to the
    // outline's own field as well.
    let fill_for = |paint: serde_json::Value| {
        let style = style_with_layer(json!({
            "id": "polys", "type": "fill", "source": "src", "source-layer": "land",
            "paint": paint
        }));
        let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("conversion");
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let text = serde_json::to_string(&recipe).unwrap();
        ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
        recipe["nodes"]
            .as_object()
            .unwrap()
            .values()
            .find(|n| n["op"] == "fill-solid")
            .expect("a fill-solid")
            .clone()
    };
    let fill = fill_for(json!({
        "fill-color": "#eeeeee", "fill-outline-color": "#333333", "fill-opacity": 0.4
    }));
    assert_eq!(fill["fill-alpha"], 0.4);
    assert_eq!(fill["edge-alpha"], 0.4);

    let opacity = json!(["interpolate", ["linear"], ["zoom"], 10, 0, 14, 0.8]);
    let outline = json!(["match", ["get", "class"], "park", "#336633", "#333333"]);
    let fill = fill_for(json!({
        "fill-color": "#eeeeee", "fill-outline-color": outline, "fill-opacity": opacity
    }));
    assert_eq!(fill["opacity-expr"], opacity);
    assert_eq!(fill["edge-alpha-expr"], opacity);

    // No outline, nothing to fade.
    let fill = fill_for(json!({ "fill-color": "#eeeeee", "fill-opacity": 0.4 }));
    assert!(fill.get("edge-alpha").is_none());
}

/// The node a single-layer style converts `layer` to (the one with `op`),
/// and the conversion's warnings.
fn node_for(layer: serde_json::Value, op: &str) -> (serde_json::Value, Vec<String>) {
    let mut style = style_with_layer(layer);
    style["glyphs"] = json!("https://example.com/fonts/{fontstack}/{range}.pbf");
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("conversion");
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
    let node = recipe["nodes"]
        .as_object()
        .unwrap()
        .values()
        .find(|n| n["op"] == op)
        .unwrap_or_else(|| panic!("a `{op}` node"))
        .clone();
    (node, report.warnings)
}

#[test]
fn colours_fall_back_to_maplibres_black() {
    // A feature a colour expression gives no colour draws the constant
    // beside it, and an unset colour draws as the constant alone: both are
    // MapLibre's default, black, for fills, extrusions and lines.
    let expr = json!(["match", ["get", "class"], "a", "#ff0000", ["get", "colour"]]);
    for (ty, prop, op) in [
        ("fill", "fill-color", "fill-solid"),
        ("fill-extrusion", "fill-extrusion-color", "fill-solid"),
        ("line", "line-color", "stroke"),
    ] {
        let field = if op == "stroke" { "color" } else { "fill" };
        let layer = |paint: serde_json::Value| json!({ "id": "l", "type": ty, "source": "src", "source-layer": "x", "paint": paint });
        let (node, warnings) = node_for(layer(json!({ prop: expr.clone() })), op);
        assert_eq!(node[field], "#000000", "{prop} behind an expression");
        assert!(warnings.is_empty(), "{prop}: {warnings:?}");

        let (node, warnings) = node_for(layer(json!({})), op);
        assert_eq!(node[field], "#000000", "{prop} unset");
        assert!(warnings.is_empty(), "{prop} unset: {warnings:?}");

        let (node, warnings) = node_for(layer(json!({ prop: 12 })), op);
        assert_eq!(node[field], "#000000", "{prop} unreadable");
        assert!(
            warnings.iter().any(|w| w.contains(prop)),
            "{prop} unreadable: {warnings:?}"
        );
    }

    // Circles and text leave the colour to the node, whose default is
    // black too; an unreadable one says so.
    let (_, warnings) = node_for(
        json!({ "id": "c", "type": "circle", "source": "src", "source-layer": "x",
                "paint": { "circle-color": 12, "circle-stroke-color": true } }),
        "circles",
    );
    for prop in ["circle-color", "circle-stroke-color"] {
        assert!(warnings.iter().any(|w| w.contains(prop)), "{warnings:?}");
    }
}

#[test]
fn a_halo_without_a_constant_colour_is_maplibres_transparent_one() {
    let label = |paint: serde_json::Value| {
        json!({ "id": "t", "type": "symbol", "source": "src", "source-layer": "x",
                "layout": { "text-field": ["get", "name"] }, "paint": paint })
    };
    // A width with no colour draws no halo in MapLibre.
    let (node, _) = node_for(label(json!({ "text-halo-width": 2 })), "text-labels");
    assert_eq!(node["halo-color"], "#00000000");
    // Behind an expression, a feature it gives no colour has no halo.
    let expr = json!(["match", ["get", "class"], "a", "#ffffff", ["get", "halo"]]);
    let (node, _) = node_for(
        label(json!({ "text-halo-width": 2, "text-halo-color": expr })),
        "text-labels",
    );
    assert_eq!(node["halo-color"], "#00000000");
    assert_eq!(node["halo-color-expr"], expr);
    // A constant colour carries over as it is.
    let (node, _) = node_for(
        label(json!({ "text-halo-width": 2, "text-halo-color": "#ffffff" })),
        "text-labels",
    );
    assert_eq!(node["halo-color"], "#ffffff");
}
