//! A `circle` layer converts to a single `circles` paint node, with each
//! paint property routed to a constant field or a `*-expr` data-driven field.

use ezu_translate::maplibre::{convert, ConvertOptions};

const STYLE: &str = r##"{
  "version": 8,
  "name": "circles",
  "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
  "layers": [
    { "id": "dots", "type": "circle", "source": "s", "source-layer": "pois",
      "paint": {
        "circle-radius": 4, "circle-color": "#ff0000",
        "circle-stroke-width": 2, "circle-stroke-color": "#ffffff"
      } }
  ]
}"##;

#[test]
fn circle_converts_to_single_circles_node() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, _) = convert(&style, &ConvertOptions::default()).unwrap();
    let nodes = recipe["nodes"].as_object().unwrap();

    // Exactly one `circles` node — no sprite/stamp path anymore.
    let circles: Vec<&serde_json::Value> =
        nodes.values().filter(|n| n["op"] == "circles").collect();
    assert_eq!(circles.len(), 1, "expected a single circles node");
    assert!(nodes
        .values()
        .all(|n| n["op"] != "circle" && n["op"] != "stamp"));

    let c = circles[0];
    assert_eq!(c["radius"], 4.0);
    assert_eq!(c["color"], "#ff0000");
    assert_eq!(c["stroke-width"], 2.0);
    assert_eq!(c["stroke-color"], "#ffffff");
    // Constant props don't leak `*-expr` fields.
    assert!(c.get("radius-expr").is_none());
    assert!(c.get("color-expr").is_none());

    // Valid Document.
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}

const DATA_DRIVEN_STYLE: &str = r##"{
  "version": 8,
  "name": "circles-dd",
  "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
  "layers": [
    { "id": "quakes", "type": "circle", "source": "s", "source-layer": "eq",
      "paint": {
        "circle-radius": ["interpolate", ["linear"], ["get", "mag"], 1, 2, 6, 20],
        "circle-color": "#3388ff"
      } }
  ]
}"##;

#[test]
fn data_driven_radius_becomes_radius_expr() {
    let style: serde_json::Value = serde_json::from_str(DATA_DRIVEN_STYLE).unwrap();
    let (recipe, _) = convert(&style, &ConvertOptions::default()).unwrap();
    let nodes = recipe["nodes"].as_object().unwrap();

    let circles: Vec<&serde_json::Value> =
        nodes.values().filter(|n| n["op"] == "circles").collect();
    assert_eq!(circles.len(), 1);
    let c = circles[0];

    // `circle-radius` is a feature-driven interpolate → `radius-expr`, with no
    // baked constant `radius`.
    assert!(c["radius-expr"].is_array(), "expected a radius-expr array");
    assert!(
        c.get("radius").is_none(),
        "no constant radius when data-driven"
    );
    // The constant color still bakes.
    assert_eq!(c["color"], "#3388ff");

    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}
