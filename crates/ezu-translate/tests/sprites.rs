//! A top-level `sprite` becomes an ezu `sprite` source; `symbol` icon
//! layers convert to `icon` + `stamp`, and `fill-pattern` to
//! `icon` + `tiling` clipped to the fill shape.

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

    // Symbol icon layer → icon + stamp; the icon names the sprite + rect.
    let icons = by_op("icon");
    assert!(icons
        .iter()
        .any(|n| n["name"] == "airport-15" && n["sprite"] == "@default"));
    let stamps = by_op("stamp");
    assert!(stamps.iter().any(|n| n["scale"] == 1.5));

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
fn data_driven_icon_size_becomes_scale_expr() {
    // An expression-valued `icon-size` routes to the `stamp` node's
    // `scale-expr` sibling instead of being dropped with a warning.
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
    let stamp = nodes
        .values()
        .find(|n| n["op"] == "stamp")
        .expect("a stamp node");
    // The raw expression carries over verbatim; no constant `scale`.
    assert_eq!(
        stamp["scale-expr"],
        serde_json::json!(["interpolate", ["linear"], ["zoom"], 10, 0.5, 16, 2])
    );
    assert!(stamp.get("scale").is_none(), "no constant scale: {stamp}");
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
fn data_driven_icon_image_becomes_stamp_name_expr() {
    // A data-driven `icon-image` (an expression, not a constant name) passes
    // the expression to `stamp` as a `name-expr` over the sheet's atlas, with
    // no per-icon `icon` node.
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
    let stamp = nodes
        .values()
        .find(|n| n["op"] == "stamp")
        .expect("a stamp node");
    // The icon-image expression carries over verbatim as `name-expr`, resolved
    // against the `default` sprite sheet.
    assert_eq!(
        stamp["name-expr"],
        serde_json::json!(["concat", ["get", "class"], "-15"])
    );
    assert_eq!(stamp["sprite"], "@default");
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
