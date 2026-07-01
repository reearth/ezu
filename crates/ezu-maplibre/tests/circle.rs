//! A `circle` layer converts to a `circle` sprite `stamp`ed at each point,
//! with `circle-stroke-*` as a larger ring stamped underneath.

use ezu_maplibre::{convert, ConvertOptions};

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
fn circle_converts_to_sprite_plus_stamp_with_ring() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, _) = convert(&style, &ConvertOptions::default()).unwrap();
    let nodes = recipe["nodes"].as_object().unwrap();

    // Two disks: the stroke ring (larger) and the fill.
    let circles: Vec<&serde_json::Value> = nodes.values().filter(|n| n["op"] == "circle").collect();
    assert_eq!(circles.len(), 2, "expected stroke + fill disks");
    assert!(circles.iter().all(|c| c["kind"] == "sprite"));

    // A stamp per disk, each placing a circle at the point features.
    let stamps: Vec<&serde_json::Value> = nodes.values().filter(|n| n["op"] == "stamp").collect();
    assert_eq!(stamps.len(), 2);
    assert!(stamps
        .iter()
        .all(|s| s["image"].as_str().unwrap().contains("_circle")));

    // The fill disk is red; a white stroke disk exists too.
    let colors: Vec<&str> = circles.iter().filter_map(|c| c["color"].as_str()).collect();
    assert!(colors.contains(&"#ff0000"));
    assert!(colors.contains(&"#ffffff"));

    // Valid Document.
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}
