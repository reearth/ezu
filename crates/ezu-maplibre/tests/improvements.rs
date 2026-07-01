//! Small-fidelity conversions: CSS named colours, `visibility: none`, and
//! per-layer zoom ranges.

use ezu_maplibre::{convert, ConvertOptions};

const STYLE: &str = r##"{
  "version": 8,
  "name": "t",
  "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
  "layers": [
    { "id": "bg", "type": "background", "paint": { "background-color": "steelblue" } },
    { "id": "hidden", "type": "fill", "source": "s", "source-layer": "a",
      "layout": { "visibility": "none" }, "paint": { "fill-color": "#123456" } },
    { "id": "lowzoom", "type": "fill", "source": "s", "source-layer": "a",
      "maxzoom": 6, "paint": { "fill-color": "red" } },
    { "id": "shown", "type": "fill", "source": "s", "source-layer": "a",
      "minzoom": 4, "paint": { "fill-color": "white" } }
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
fn named_colors_visibility_and_zoom_range() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, _) = convert(
        &style,
        &ConvertOptions {
            zoom: Some(10.0),
            ..Default::default()
        },
    )
    .unwrap();

    // Named colour on background resolves (steelblue → #4682b4).
    let bg = recipe["nodes"]
        .as_object()
        .unwrap()
        .values()
        .find(|n| n["op"] == "solid")
        .unwrap();
    assert_eq!(bg["color"], "#4682b4");

    let fills = fills(&recipe);
    // `visibility: none` layer dropped.
    assert!(
        !fills.contains(&"#123456".to_string()),
        "hidden layer emitted"
    );
    // `maxzoom: 6` layer dropped at z10.
    assert!(
        !fills.contains(&"#ff0000".to_string()),
        "out-of-range layer emitted"
    );
    // In-range layer present (white → #ffffff).
    assert!(
        fills.contains(&"#ffffff".to_string()),
        "in-range layer missing"
    );
}

#[test]
fn zoom_range_keeps_layer_when_in_range() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    // At z5 the `maxzoom: 6` red layer is in range and should appear.
    let (recipe, _) = convert(
        &style,
        &ConvertOptions {
            zoom: Some(5.0),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(fills(&recipe).contains(&"#ff0000".to_string()));
}
