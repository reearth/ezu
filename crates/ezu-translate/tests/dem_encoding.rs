//! A MapLibre `raster-dem` `encoding` maps onto the encoding ezu's `dem`
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

// MapLibre subtracts `baseShift`, and its own Terrain-RGB and Terrarium
// shifts are the positive 10000 and 32768.
#[test]
fn custom_encoding_equal_to_mapbox_maps_exactly() {
    let (recipe, warnings) = translate(json!({
        "encoding": "custom",
        "redFactor": 6553.6, "greenFactor": 25.6, "blueFactor": 0.1, "baseShift": 10000
    }));
    assert_eq!(encoding(&recipe), "mapbox-rgb");
    assert!(recipe["sources"]["terrain"].get("red-factor").is_none());
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn custom_encoding_equal_to_terrarium_maps_exactly() {
    let (recipe, warnings) = translate(json!({
        "encoding": "custom",
        "redFactor": 256, "greenFactor": 1, "blueFactor": 0.00390625, "baseShift": 32768
    }));
    assert_eq!(encoding(&recipe), "terrarium");
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn other_custom_factors_carry_over() {
    let (recipe, warnings) = translate(json!({
        "encoding": "custom",
        "redFactor": 2.0, "greenFactor": 1.0, "blueFactor": 1.0, "baseShift": 5
    }));
    let terrain = &recipe["sources"]["terrain"];
    assert_eq!(encoding(&recipe), "custom");
    assert_eq!(terrain["red-factor"], 2.0);
    assert_eq!(terrain["green-factor"], 1.0);
    assert_eq!(terrain["blue-factor"], 1.0);
    assert_eq!(terrain["base-shift"], 5.0);
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn mapbox_factors_with_a_negated_shift_stay_custom() {
    // Heights 20000 m above Terrain-RGB's, which is what MapLibre draws.
    let (recipe, _) = translate(json!({
        "encoding": "custom",
        "redFactor": 6553.6, "greenFactor": 25.6, "blueFactor": 0.1, "baseShift": -10000
    }));
    assert_eq!(encoding(&recipe), "custom");
    assert_eq!(recipe["sources"]["terrain"]["base-shift"], -10000.0);
}

#[test]
fn unset_custom_factors_are_left_to_the_shared_defaults() {
    let (recipe, warnings) = translate(json!({ "encoding": "custom", "redFactor": 3 }));
    let terrain = recipe["sources"]["terrain"].as_object().unwrap();
    assert_eq!(terrain["encoding"], "custom");
    assert_eq!(terrain["red-factor"], 3.0);
    assert!(!terrain.contains_key("base-shift"));
    assert!(warnings.is_empty(), "{warnings:?}");
}
