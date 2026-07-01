//! `fill-extrusion` converts to a plain 2-D footprint fill (ezu has no
//! 3-D camera); height/base are dropped, `fill-extrusion-color` is used.

use ezu_maplibre::{convert, ConvertOptions};

const STYLE: &str = r##"{
  "version": 8,
  "name": "buildings",
  "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
  "layers": [
    { "id": "bldg", "type": "fill-extrusion", "source": "s", "source-layer": "building",
      "paint": {
        "fill-extrusion-color": "#cccccc",
        "fill-extrusion-height": ["get", "render_height"],
        "fill-extrusion-opacity": 0.8
      } }
  ]
}"##;

#[test]
fn fill_extrusion_becomes_a_flat_fill() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, _) = convert(&style, &ConvertOptions::default()).unwrap();
    let nodes = recipe["nodes"].as_object().unwrap();

    // One fill-solid painting the footprint with the extrusion colour + opacity.
    let fills: Vec<&serde_json::Value> =
        nodes.values().filter(|n| n["op"] == "fill-solid").collect();
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0]["fill"], "#cccccc");
    assert_eq!(fills[0]["fill-alpha"], 0.8);
    // No 3-D concepts leak into the recipe.
    assert!(fills[0].get("edge").is_none());
    assert!(!serde_json::to_string(&recipe).unwrap().contains("height"));

    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}
