//! A top-level `sprite` becomes an ezu `sprite` source; a `symbol` layer's
//! `icon-image` rides its label node (so the icon joins the shared collision
//! index), and `fill-pattern` converts to `icon` + `tiling` clipped to the
//! fill shape.

use ezu_translate::maplibre::{convert, ConvertOptions};

const STYLE: &str = r##"{
  "version": 8,
  "name": "sprites",
  "sprite": "https://example.com/sprites/basemap",
  "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
  "layers": [
    { "id": "pois", "type": "symbol", "source": "s", "source-layer": "poi",
      "layout": { "icon-image": "airport-15", "icon-size": 1.5 } },
    { "id": "hatch", "type": "fill", "source": "s", "source-layer": "landuse",
      "paint": { "fill-pattern": "hatch-16" } },
    { "id": "dashes", "type": "line", "source": "s", "source-layer": "roads",
      "paint": { "line-pattern": "dash-8", "line-width": 6 } },
    { "id": "labels", "type": "symbol", "source": "s", "source-layer": "place",
      "layout": { "text-field": "{name}" } }
  ]
}"##;

#[test]
fn sprite_source_icons_and_pattern_convert() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, report) = convert(&style, &ConvertOptions::default()).unwrap();

    // A single-URL `sprite` becomes the `default` sheet with derived URLs.
    let sources = recipe["sources"].as_object().unwrap();
    assert_eq!(sources["default"]["type"], "sprite");
    assert_eq!(
        sources["default"]["image"],
        "https://example.com/sprites/basemap.png"
    );
    assert_eq!(
        sources["default"]["index"],
        "https://example.com/sprites/basemap.json"
    );

    let nodes = recipe["nodes"].as_object().unwrap();
    let by_op = |op: &str| -> Vec<&serde_json::Value> {
        nodes.values().filter(|n| n["op"] == op).collect()
    };

    // Symbol icon layer → a label node carrying the icon, so it collides
    // with every other symbol layer instead of being stamped blindly.
    let labels = by_op("text-labels");
    assert!(
        labels.iter().any(|n| n["icon-name"] == "airport-15"
            && n["icon-sprite"] == "@default"
            && n["icon-size"] == 1.5),
        "expected an icon-carrying label node: {labels:?}"
    );
    assert!(by_op("stamp").is_empty(), "no bare stamp for a point icon");

    let icons = by_op("icon");

    // fill-pattern → fill-solid shape + icon + tiling + blend(clip:true).
    assert!(icons.iter().any(|n| n["name"] == "hatch-16"));
    assert!(by_op("tiling")
        .iter()
        .any(|n| n["input"].as_str().unwrap().contains("__icon")));
    let blends = by_op("blend");
    assert!(blends.iter().any(|n| n["clip"] == true));

    // line-pattern → icon + line-stamp fit to the stroke width.
    assert!(icons.iter().any(|n| n["name"] == "dash-8"));
    let line_stamps = by_op("line-stamp");
    assert_eq!(line_stamps.len(), 1);
    assert_eq!(line_stamps[0]["width-px"], 6.0);
    assert!(line_stamps[0]["image"].as_str().unwrap().contains("__icon"));

    // The text-only symbol layer is reported, not silently dropped.
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("text-only") || w.contains("labels")),
        "expected a text-label warning: {:?}",
        report.warnings
    );

    // Valid ezu Document.
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}

#[test]
fn data_driven_icon_size_becomes_size_expr() {
    // An expression-valued `icon-size` routes to the label node's
    // `icon-size-expr` sibling instead of being dropped with a warning.
    const STYLE: &str = r##"{
      "version": 8,
      "name": "dd-icon",
      "sprite": "https://example.com/sprites/basemap",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "pois", "type": "symbol", "source": "s", "source-layer": "poi",
          "layout": {
            "icon-image": "airport-15",
            "icon-size": ["interpolate", ["linear"], ["zoom"], 10, 0.5, 16, 2]
          } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, report) = convert(&style, &ConvertOptions::default()).unwrap();

    let nodes = recipe["nodes"].as_object().unwrap();
    let labels = nodes
        .values()
        .find(|n| n["op"] == "text-labels")
        .expect("a label node");
    // The raw expression carries over verbatim; no constant `icon-size`.
    assert_eq!(
        labels["icon-size-expr"],
        serde_json::json!(["interpolate", ["linear"], ["zoom"], 10, 0.5, 16, 2])
    );
    assert!(
        labels.get("icon-size").is_none(),
        "no constant icon-size: {labels}"
    );
    // No "not supported" warning about a dropped data-driven size.
    assert!(
        !report.warnings.iter().any(|w| w.contains("icon-size")),
        "data-driven icon-size should not warn: {:?}",
        report.warnings
    );

    // Valid ezu Document.
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}

#[test]
fn data_driven_icon_image_becomes_icon_name_expr() {
    // A data-driven `icon-image` (an expression, not a constant name) passes
    // the expression to the label node as an `icon-name-expr` over the
    // sheet's atlas, with no per-icon `icon` node.
    const STYLE: &str = r##"{
      "version": 8,
      "name": "dd-icon-image",
      "sprite": "https://example.com/sprites/basemap",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "pois", "type": "symbol", "source": "s", "source-layer": "poi",
          "layout": { "icon-image": ["concat", ["get", "class"], "-15"] } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, _report) = convert(&style, &ConvertOptions::default()).unwrap();

    let nodes = recipe["nodes"].as_object().unwrap();
    let labels = nodes
        .values()
        .find(|n| n["op"] == "text-labels")
        .expect("a label node");
    // The icon-image expression carries over verbatim as `icon-name-expr`,
    // resolved against the `default` sprite sheet.
    assert_eq!(
        labels["icon-name-expr"],
        serde_json::json!(["concat", ["get", "class"], "-15"])
    );
    assert_eq!(labels["icon-sprite"], "@default");
    // No up-front `icon` node — cropping is per-feature at eval time.
    assert!(
        !nodes.values().any(|n| n["op"] == "icon"),
        "data-driven icon-image should not emit an `icon` node: {nodes:?}"
    );

    // Valid ezu Document.
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}

#[test]
fn data_driven_icon_image_without_sprite_warns() {
    // Without a top-level `sprite`, a data-driven `icon-image` has no sheet to
    // crop from, so the layer is reported and skipped rather than emitting an
    // unresolvable stamp.
    const STYLE: &str = r##"{
      "version": 8,
      "name": "dd-icon-no-sprite",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "pois", "type": "symbol", "source": "s", "source-layer": "poi",
          "layout": { "icon-image": ["get", "maki"] } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, report) = convert(&style, &ConvertOptions::default()).unwrap();

    assert!(
        recipe["nodes"]
            .as_object()
            .unwrap()
            .values()
            .all(|n| n["op"] != "stamp"),
        "no stamp should be emitted without a sprite"
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("icon-image") && w.contains("sprite")),
        "expected a data-driven icon-image sprite warning: {:?}",
        report.warnings
    );
}

#[test]
fn inline_sprite_index_parses() {
    // A recipe author can inline the index instead of a URL.
    let recipe = serde_json::json!({
        "name": "inline-sprite",
        "output": "out",
        "sources": {
            "sp": {
                "type": "sprite",
                "image": "file:atlas.png",
                "index": { "dot": { "x": 0, "y": 0, "width": 8, "height": 8, "pixelRatio": 2 } }
            }
        },
        "nodes": {
            "ic": { "op": "icon", "sprite": "@sp", "name": "dot" },
            "pts": { "op": "point-grid", "spacing-px": 64 },
            "out": { "op": "stamp", "features": "@pts", "image": "@ic" }
        }
    });
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("inline sprite index parses as ezu Document");
}
