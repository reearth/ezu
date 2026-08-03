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
    // MapLibre's `maxzoom` is exclusive and ezu's `max-zoom` is not, so
    // `maxzoom: 6` draws through z5 — `max-zoom: 5`, not 6.
    assert!(
        feats.iter().any(|f| f["max-zoom"] == 5),
        "expected a features node with max-zoom 5 for maxzoom 6: {feats:?}"
    );
    assert!(
        !feats.iter().any(|f| f["max-zoom"] == 6),
        "max-zoom 6 would draw z6, which MapLibre hides: {feats:?}"
    );
    // `minzoom` is inclusive on both sides, so it carries over unchanged.
    assert!(
        feats.iter().any(|f| f["min-zoom"] == 4),
        "expected a features node with min-zoom 4: {feats:?}"
    );
}

/// The bounds that need arithmetic rather than a copy: fractional bounds
/// are thresholds, and a band that holds no whole zoom is a layer MapLibre
/// never draws.
#[test]
fn zoom_bounds_convert_from_a_half_open_range() {
    let case = |extra: &str| {
        let style: serde_json::Value = serde_json::from_str(&format!(
            r##"{{
              "version": 8, "name": "t",
              "sources": {{ "s": {{ "type": "vector", "url": "https://example.com/t.json" }} }},
              "layers": [{{ "id": "l", "type": "fill", "source": "s", "source-layer": "a",
                            {extra} "paint": {{ "fill-color": "red" }} }}]
            }}"##
        ))
        .unwrap();
        let (recipe, report) = convert(&style, &ConvertOptions::default()).unwrap();
        let f = features(&recipe);
        let gate = f
            .first()
            .map(|f| (f["min-zoom"].as_u64(), f["max-zoom"].as_u64()));
        (gate, report.warnings.len())
    };

    // `z < 12.5` last shows at z12; `z >= 12.4` first shows at z13. Rounding
    // would put each of these a level out.
    assert_eq!(case(r#""maxzoom": 12.5,"#).0, Some((None, Some(12))));
    assert_eq!(case(r#""minzoom": 12.4,"#).0, Some((Some(13), None)));
    // A whole bound still steps down by one.
    assert_eq!(case(r#""maxzoom": 12,"#).0, Some((None, Some(11))));
    // Both ends together.
    assert_eq!(
        case(r#""minzoom": 4, "maxzoom": 9,"#).0,
        Some((Some(4), Some(8)))
    );

    // `z < 0` is empty, as is `12 <= z < 12`: MapLibre draws neither, so the
    // layer is dropped with a warning rather than gated to nothing.
    for degenerate in [
        r#""maxzoom": 0,"#,
        r#""minzoom": 12, "maxzoom": 12,"#,
        r#""minzoom": 9, "maxzoom": 4,"#,
    ] {
        let (gate, warnings) = case(degenerate);
        assert_eq!(gate, None, "{degenerate} should emit no features node");
        assert!(warnings > 0, "{degenerate} should be reported");
    }

    // `maxzoom: 1` keeps z0 only — the smallest band that still draws.
    assert_eq!(case(r#""maxzoom": 1,"#).0, Some((None, Some(0))));
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
