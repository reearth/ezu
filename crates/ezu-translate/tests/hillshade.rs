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
