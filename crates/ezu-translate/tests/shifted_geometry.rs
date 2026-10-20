//! Paint properties that move a layer's geometry rather than colour it:
//! `circle-translate` / `fill-translate` / `line-translate` become a
//! `transform` node shifting the features by that many pixels.

use ezu_translate::maplibre::{convert, ConvertOptions};
use serde_json::{json, Value};

/// Convert a style holding the one `layer`; the recipe's nodes and the
/// warnings.
fn convert_layer(layer: Value) -> (serde_json::Map<String, Value>, Vec<String>) {
    let style = json!({
        "version": 8,
        "name": "shifted",
        "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
        "layers": [layer]
    });
    let (recipe, report) = convert(&style, &ConvertOptions::default()).unwrap();
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
    (
        recipe["nodes"].as_object().unwrap().clone(),
        report.warnings,
    )
}

/// The node the paint node `paint_id` reads its features from.
fn upstream<'a>(nodes: &'a serde_json::Map<String, Value>, paint_id: &str) -> &'a Value {
    let src = nodes[paint_id]["features"].as_str().unwrap();
    &nodes[src.trim_start_matches('@')]
}

#[test]
fn translate_shifts_circles_fills_and_lines_by_pixels() {
    for (ty, prop, paint_id, paint) in [
        (
            "circle",
            "circle-translate",
            "l__circles",
            json!({ "circle-radius": 4 }),
        ),
        (
            "fill",
            "fill-translate",
            "l__fill",
            json!({ "fill-color": "#888888" }),
        ),
        (
            "line",
            "line-translate",
            "l__stroke",
            json!({ "line-color": "#888888" }),
        ),
    ] {
        let mut paint = paint;
        paint[prop] = json!([3, -2]);
        // The anchor only matters on a rotated map; a tile never is.
        paint[format!("{prop}-anchor")] = json!("viewport");
        let (nodes, warnings) = convert_layer(json!({
            "id": "l", "type": ty, "source": "s", "source-layer": "x", "paint": paint
        }));
        assert!(warnings.is_empty(), "{ty}: {warnings:?}");
        let shift = upstream(&nodes, paint_id);
        assert_eq!(shift["op"], "transform", "{ty}");
        assert_eq!(shift["translate-px"], json!([3.0, -2.0]), "{ty}");
    }
}

#[test]
fn a_zero_or_literal_translate_needs_no_guessing() {
    let (nodes, warnings) = convert_layer(json!({
        "id": "l", "type": "circle", "source": "s", "source-layer": "x",
        "paint": { "circle-translate": [0, 0] }
    }));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(upstream(&nodes, "l__circles")["op"], "features");

    let (nodes, warnings) = convert_layer(json!({
        "id": "l", "type": "circle", "source": "s", "source-layer": "x",
        "paint": { "circle-translate": ["literal", [1, 2]] }
    }));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        upstream(&nodes, "l__circles")["translate-px"],
        json!([1.0, 2.0])
    );
}

#[test]
fn an_expression_translate_warns_and_draws_unshifted() {
    let (nodes, warnings) = convert_layer(json!({
        "id": "l", "type": "circle", "source": "s", "source-layer": "x",
        "paint": { "circle-translate": ["interpolate", ["linear"], ["zoom"], 10, ["literal", [0, 0]], 16, ["literal", [4, 4]]] }
    }));
    assert_eq!(upstream(&nodes, "l__circles")["op"], "features");
    assert!(
        warnings.iter().any(|w| w.contains("circle-translate")),
        "{warnings:?}"
    );
}
