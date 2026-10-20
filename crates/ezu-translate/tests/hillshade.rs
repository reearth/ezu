//! A MapLibre `hillshade` layer over a `raster-dem` source lowers to an
//! ezu `dem` + `hillshade` node pair, and the recipe stays a valid Document.

use ezu_translate::maplibre::{convert, ConvertOptions};

const STYLE: &str = r##"{
  "version": 8,
  "name": "hs",
  "sources": {
    "terrain": {
      "type": "raster-dem",
      "tiles": ["https://example.com/dem/{z}/{x}/{y}.webp"],
      "encoding": "terrarium",
      "tileSize": 512,
      "maxzoom": 14
    }
  },
  "layers": [
    { "id": "bg", "type": "background", "paint": { "background-color": "#ffffff" } },
    { "id": "hills", "type": "hillshade", "source": "terrain",
      "paint": { "hillshade-exaggeration": 0.5, "hillshade-illumination-direction": 335 } }
  ]
}"##;

#[test]
fn hillshade_layer_lowers_to_dem_and_hillshade_nodes() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("convert");
    assert!(
        report.warnings.is_empty(),
        "unexpected warnings: {:?}",
        report.warnings
    );

    // raster-dem → ezu dem source carrying the stitch/zoom hints.
    let dem_src = &recipe["sources"]["terrain"];
    assert_eq!(dem_src["type"], "dem");
    assert_eq!(dem_src["encoding"], "terrarium");
    assert_eq!(dem_src["neighbor-fetch"], true);
    assert_eq!(dem_src["max-zoom"], 14);

    let nodes = recipe["nodes"].as_object().unwrap();
    let dem = nodes
        .values()
        .find(|n| n["op"] == "dem")
        .expect("a dem node");
    assert_eq!(dem["source"], "terrain");
    let hs = nodes
        .values()
        .find(|n| n["op"] == "hillshade")
        .expect("a hillshade node");
    assert_eq!(hs["azimuth-deg"], 335.0);
    assert_eq!(hs["exaggeration"], 0.5);
    assert!(hs["field"].as_str().unwrap().starts_with('@'));

    // Still a valid ezu Document.
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}

fn hillshade_with_paint(paint: serde_json::Value) -> (serde_json::Value, Vec<String>) {
    let mut style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    style["layers"][1]["paint"] = paint;
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("convert");
    let text = serde_json::to_string(&recipe).unwrap();
    let doc = ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
    ezu_graph::build_graph(&doc, &ezu_paint::nodes::default_registry()).expect("builds");
    let hs = recipe["nodes"]["hills__hillshade"].clone();
    (hs, report.warnings)
}

#[test]
fn shadow_and_highlight_colours_carry_over() {
    let (hs, warnings) = hillshade_with_paint(serde_json::json!({
        "hillshade-shadow-color": "rgba(10, 20, 30, 0.5)",
        "hillshade-highlight-color": "white"
    }));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(hs["shadow-color"], "#0a141e80");
    assert_eq!(hs["highlight-color"], "#ffffff");
}

#[test]
fn unset_colours_keep_the_node_defaults() {
    let (hs, _) = hillshade_with_paint(serde_json::json!({}));
    assert!(hs.get("shadow-color").is_none());
    assert!(hs.get("highlight-color").is_none());
}

#[test]
fn a_colour_expression_warns_and_keeps_the_default() {
    let (hs, warnings) = hillshade_with_paint(serde_json::json!({
        "hillshade-shadow-color": ["interpolate", ["linear"], ["zoom"], 5, "#000000", 10, "#333333"]
    }));
    assert!(hs.get("shadow-color").is_none());
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("hillshade-shadow-color")),
        "{warnings:?}"
    );
}
