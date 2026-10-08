//! A MapLibre `raster-dem` `encoding` maps onto the two encodings ezu's `dem`
//! source accepts, and the translated recipe parses as an ezu Document.

use ezu_translate::maplibre::{convert, ConvertOptions};
use serde_json::{json, Value};

fn translate(extra: Value) -> (Value, Vec<String>) {
    let mut dem = json!({
        "type": "raster-dem",
        "tiles": ["https://example.com/dem/{z}/{x}/{y}.png"]
    });
    dem.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let style = json!({
        "version": 8,
        "sources": { "terrain": dem },
        "layers": [
            { "id": "bg", "type": "background", "paint": { "background-color": "#ffffff" } },
            { "id": "hills", "type": "hillshade", "source": "terrain" }
        ]
    });
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("convert");
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
    (recipe, report.warnings)
}

fn encoding(recipe: &Value) -> &str {
    recipe["sources"]["terrain"]["encoding"].as_str().unwrap()
}

#[test]
fn absent_encoding_is_mapbox_rgb() {
    let (recipe, warnings) = translate(json!({}));
    assert_eq!(encoding(&recipe), "mapbox-rgb");
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn mapbox_encoding_is_mapbox_rgb() {
    let (recipe, warnings) = translate(json!({ "encoding": "mapbox" }));
    assert_eq!(encoding(&recipe), "mapbox-rgb");
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn terrarium_encoding_is_kept() {
    let (recipe, warnings) = translate(json!({ "encoding": "terrarium" }));
    assert_eq!(encoding(&recipe), "terrarium");
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn custom_encoding_equal_to_mapbox_maps_exactly() {
    let (recipe, warnings) = translate(json!({
        "encoding": "custom",
        "redFactor": 6553.6, "greenFactor": 25.6, "blueFactor": 0.1, "baseShift": -10000
    }));
    assert_eq!(encoding(&recipe), "mapbox-rgb");
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn custom_encoding_equal_to_terrarium_maps_exactly() {
    let (recipe, warnings) = translate(json!({
        "encoding": "custom",
        "redFactor": 256, "greenFactor": 1, "blueFactor": 0.00390625, "baseShift": -32768
    }));
    assert_eq!(encoding(&recipe), "terrarium");
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn custom_encoding_without_equivalent_warns_and_falls_back() {
    let (recipe, warnings) = translate(json!({
        "encoding": "custom",
        "redFactor": 2.0, "greenFactor": 1.0, "blueFactor": 1.0, "baseShift": 5
    }));
    assert_eq!(encoding(&recipe), "mapbox-rgb");
    assert!(
        warnings.iter().any(|w| w.contains("custom raster-dem")),
        "{warnings:?}"
    );
}
