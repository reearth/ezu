//! Small-fidelity conversions: CSS named colours, `visibility: none`, and
//! per-layer zoom ranges → the `features` node's `min-zoom`/`max-zoom`.

use ezu_translate::maplibre::{convert, ConvertOptions};

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

fn features(recipe: &serde_json::Value) -> Vec<serde_json::Value> {
    recipe["nodes"]
        .as_object()
        .unwrap()
        .values()
        .filter(|n| n["op"] == "features")
        .cloned()
        .collect()
}

#[test]
fn named_colors_and_visibility() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, _) = convert(&style, &ConvertOptions::default()).unwrap();

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
    // Both zoom-ranged layers are now always emitted (recipes are
    // zoom-independent; the range becomes a render-time gate).
    assert!(fills.contains(&"#ff0000".to_string()), "red layer missing");
    assert!(
        fills.contains(&"#ffffff".to_string()),
        "white layer missing"
    );
}

#[test]
fn zoom_range_becomes_features_gate() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, _) = convert(&style, &ConvertOptions::default()).unwrap();

    let feats = features(&recipe);
    // `maxzoom: 6` → a features node with `max-zoom: 6`.
    assert!(
        feats.iter().any(|f| f["max-zoom"] == 6),
        "expected a features node with max-zoom 6: {feats:?}"
    );
    // `minzoom: 4` → a features node with `min-zoom: 4`.
    assert!(
        feats.iter().any(|f| f["min-zoom"] == 4),
        "expected a features node with min-zoom 4: {feats:?}"
    );
}

/// A style that seeds the layer list with a redundant duplicate background
/// must not stack the background twice; the composite starts from the
/// background node directly.
#[test]
fn redundant_leading_background_does_not_emit_a_self_blend() {
    const DUP_BG: &str = r##"{
      "version": 8,
      "name": "t",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "bg", "type": "background", "paint": { "background-color": "steelblue" } },
        { "id": "bg", "type": "background", "paint": { "background-color": "steelblue" } },
        { "id": "shown", "type": "fill", "source": "s", "source-layer": "a",
          "paint": { "fill-color": "white" } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(DUP_BG).unwrap();
    let (recipe, _) = convert(&style, &ConvertOptions::default()).unwrap();

    let nodes = recipe["nodes"].as_object().unwrap();
    // The composite stacks the background once, then the fill on top — the
    // duplicate opaque background is collapsed away, not stacked twice.
    assert_eq!(recipe["output"], "stack");
    assert_eq!(
        nodes["stack"]["layers"],
        serde_json::json!(["@bg__bg", "@shown__fill"])
    );
}
