//! `keep_hidden`: `visibility:none` layers are dropped by default, or kept
//! (gated off behind a `switch`) when the option is set.

use ezu_maplibre::{convert, ConvertOptions};

const STYLE: &str = r##"{
  "version": 8,
  "name": "kh",
  "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
  "layers": [
    { "id": "base", "type": "fill", "source": "s", "source-layer": "earth",
      "paint": { "fill-color": "#111111" } },
    { "id": "hid", "type": "fill", "source": "s", "source-layer": "water",
      "layout": { "visibility": "none" }, "paint": { "fill-color": "#222222" } }
  ]
}"##;

fn fills(recipe: &serde_json::Value) -> Vec<String> {
    recipe["nodes"]
        .as_object()
        .unwrap()
        .values()
        .filter(|n| n["op"] == "fill-solid")
        .filter_map(|n| n["fill"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn hidden_dropped_by_default() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, _) = convert(&style, &ConvertOptions::default()).unwrap();
    assert!(
        !fills(&recipe).contains(&"#222222".to_string()),
        "hidden fill emitted"
    );
    // No switch/off scaffolding when dropping.
    let nodes = recipe["nodes"].as_object().unwrap();
    assert!(!nodes.values().any(|n| n["op"] == "switch"));
}

#[test]
fn hidden_kept_and_gated_when_requested() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = ConvertOptions {
        keep_hidden: true,
        ..Default::default()
    };
    let (recipe, _) = convert(&style, &opts).unwrap();
    let nodes = recipe["nodes"].as_object().unwrap();

    // The hidden layer's fill node is kept.
    assert!(
        fills(&recipe).contains(&"#222222".to_string()),
        "hidden fill missing"
    );
    // A transparent off-branch and a switch defaulting to it exist.
    assert!(nodes.contains_key("__hidden_off"));
    let sw = nodes
        .values()
        .find(|n| n["op"] == "switch")
        .expect("a switch node");
    assert_eq!(sw["select"], "a"); // off by default
    assert_eq!(sw["a"], "@__hidden_off");
    assert!(sw["b"].as_str().unwrap().starts_with('@'));

    // The visible base layer is NOT gated (still a plain fill in outputs).
    assert!(fills(&recipe).contains(&"#111111".to_string()));

    // Still a valid ezu Document.
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}
