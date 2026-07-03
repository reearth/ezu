//! A style with two vector sources emits both, and each data layer's
//! `features` node targets the right `(source, layer)`.

use ezu_translate::maplibre::{convert, ConvertOptions};

const STYLE: &str = r##"{
  "version": 8,
  "name": "multi",
  "sources": {
    "base": { "type": "vector", "url": "https://example.com/base/tiles.json" },
    "overlay": { "type": "vector", "tiles": ["https://example.com/ov/{z}/{x}/{y}.pbf"] }
  },
  "layers": [
    { "id": "land", "type": "fill", "source": "base", "source-layer": "earth",
      "paint": { "fill-color": "#111111" } },
    { "id": "flood", "type": "fill", "source": "overlay", "source-layer": "water",
      "paint": { "fill-color": "#222222" } }
  ]
}"##;

#[test]
fn both_vector_sources_convert() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("convert");
    assert!(
        report.warnings.is_empty(),
        "unexpected: {:?}",
        report.warnings
    );

    // Both vector sources are emitted as ezu mvt sources.
    let sources = recipe["sources"].as_object().unwrap();
    assert_eq!(sources["base"]["type"], "mvt");
    assert_eq!(sources["overlay"]["type"], "mvt");

    // Each fill's features node targets its own (source, layer).
    let pairs: Vec<(String, String)> = recipe["nodes"]
        .as_object()
        .unwrap()
        .values()
        .filter(|n| n["op"] == "features")
        .map(|n| {
            (
                n["source"].as_str().unwrap().to_string(),
                n["layer"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(
        pairs.contains(&("base".into(), "earth".into())),
        "{pairs:?}"
    );
    assert!(
        pairs.contains(&("overlay".into(), "water".into())),
        "{pairs:?}"
    );

    // Still a valid ezu Document.
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}
