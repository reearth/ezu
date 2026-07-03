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
