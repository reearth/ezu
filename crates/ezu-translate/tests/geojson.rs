//! An inline `geojson` source converts to an ezu `geojson` source, and a
//! layer drawn on it targets `(source, source)` — the source is its own
//! single feature layer (no `source-layer`).

use ezu_translate::maplibre::{convert, ConvertOptions};

const STYLE: &str = r##"{
  "version": 8,
  "name": "inline-geojson",
  "sources": {
    "pts": {
      "type": "geojson",
      "data": {
        "type": "FeatureCollection",
        "features": [
          { "type": "Feature", "properties": { "kind": "a" },
            "geometry": { "type": "Polygon",
              "coordinates": [[[0,0],[1,0],[1,1],[0,1],[0,0]]] } }
        ]
      }
    },
    "remote": { "type": "geojson", "data": "https://example.com/x.geojson" }
  },
  "layers": [
    { "id": "poly", "type": "fill", "source": "pts",
      "paint": { "fill-color": "#00ff00" } },
    { "id": "rem", "type": "line", "source": "remote",
      "paint": { "line-color": "#ff0000" } }
  ]
}"##;

#[test]
fn inline_geojson_source_and_layer_convert() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, report) = convert(&style, &ConvertOptions::default()).unwrap();

    // Inline `data` is carried through; the remote form keeps its `url`.
    let sources = recipe["sources"].as_object().unwrap();
    assert_eq!(sources["pts"]["type"], "geojson");
    assert!(sources["pts"]["data"].is_object());
    assert_eq!(sources["remote"]["type"], "geojson");
    assert_eq!(sources["remote"]["url"], "https://example.com/x.geojson");

    // A geojson layer targets `(source, source)` — no `source-layer` needed.
    let nodes = recipe["nodes"].as_object().unwrap();
    let feats: Vec<&serde_json::Value> = nodes.values().filter(|n| n["op"] == "features").collect();
    assert!(feats
        .iter()
        .any(|f| f["source"] == "pts" && f["layer"] == "pts"));
    assert!(feats
        .iter()
        .any(|f| f["source"] == "remote" && f["layer"] == "remote"));

    // No spurious "unconverted source" warnings for the geojson layers.
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.contains("not a converted")),
        "unexpected warnings: {:?}",
        report.warnings
    );

    // Valid ezu Document.
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}
