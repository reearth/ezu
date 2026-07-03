//! `symbol` text layers → the `text` node: font-source mapping,
//! `{token}` rewriting, constant-vs-expression routing, icon+text.

use std::collections::HashMap;

use ezu_translate::maplibre::{convert, ConvertOptions};

fn opts_with_fonts(entries: &[(&str, &str)]) -> ConvertOptions {
    ConvertOptions {
        fonts: entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>(),
        ..ConvertOptions::default()
    }
}

#[test]
fn text_only_symbol_layer_converts_to_a_text_node() {
    const STYLE: &str = r##"{
      "version": 8,
      "name": "labels",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "places", "type": "symbol", "source": "s", "source-layer": "place",
          "layout": {
            "text-field": ["get", "name"],
            "text-font": ["Noto Sans Regular"],
            "text-size": 14,
            "text-anchor": "top",
            "text-offset": [0, 0.6],
            "text-max-width": 8,
            "text-transform": "uppercase",
            "text-letter-spacing": 0.05,
            "text-line-height": 1.3
          },
          "paint": {
            "text-color": "#334455",
            "text-halo-color": "#ffffff",
            "text-halo-width": 1.5,
            "text-opacity": 0.9
          } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[("Noto Sans Regular", "https://fonts.example/NotoSans.ttf")]);
    let (recipe, report) = convert(&style, &opts).unwrap();

    // The mapped fontstack entry became a `font` source named after it.
    let sources = recipe["sources"].as_object().unwrap();
    assert_eq!(sources["noto-sans-regular"]["type"], "font");
    assert_eq!(
        sources["noto-sans-regular"]["url"],
        "https://fonts.example/NotoSans.ttf"
    );

    let nodes = recipe["nodes"].as_object().unwrap();
    let text = nodes
        .values()
        .find(|n| n["op"] == "text")
        .expect("a text node");
    assert_eq!(text["font"], serde_json::json!(["noto-sans-regular"]));
    assert_eq!(text["text"], serde_json::json!(["get", "name"]));
    assert_eq!(text["size"], 14.0);
    assert_eq!(text["anchor"], "top");
    assert_eq!(text["offset-em"], serde_json::json!([0.0, 0.6]));
    assert_eq!(text["max-width-em"], 8.0);
    assert_eq!(text["transform"], "uppercase");
    assert_eq!(text["letter-spacing-em"], 0.05);
    assert_eq!(text["line-height"], 1.3);
    assert_eq!(text["color"], "#334455");
    assert_eq!(text["halo-color"], "#ffffff");
    assert_eq!(text["halo-width"], 1.5);
    assert_eq!(text["opacity"], 0.9);

    // No skipped-text warning.
    assert!(
        !report.warnings.iter().any(|w| w.contains("text skipped")),
        "unexpected warnings: {:?}",
        report.warnings
    );

    // Valid ezu Document.
    let doc_text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&doc_text).expect("recipe parses as ezu Document");
}

#[test]
fn token_text_field_rewrites_to_an_expression() {
    const STYLE: &str = r##"{
      "version": 8,
      "name": "tokens",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "bare", "type": "symbol", "source": "s", "source-layer": "place",
          "layout": { "text-field": "{name}", "text-font": ["F"] } },
        { "id": "mixed", "type": "symbol", "source": "s", "source-layer": "peak",
          "layout": { "text-field": "{name} ({ele} m)", "text-font": ["F"] } },
        { "id": "plain", "type": "symbol", "source": "s", "source-layer": "sea",
          "layout": { "text-field": "Ocean", "text-font": ["F"] } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[("F", "https://fonts.example/F.ttf")]);
    let (recipe, _) = convert(&style, &opts).unwrap();

    let nodes = recipe["nodes"].as_object().unwrap();
    // A bare `{name}` becomes a single to-string/get.
    assert_eq!(
        nodes["bare__text"]["text"],
        serde_json::json!(["to-string", ["get", "name"]])
    );
    // Mixed literal + tokens become a concat.
    assert_eq!(
        nodes["mixed__text"]["text"],
        serde_json::json!([
            "concat",
            ["to-string", ["get", "name"]],
            " (",
            ["to-string", ["get", "ele"]],
            " m)"
        ])
    );
    // No tokens: the constant carries through.
    assert_eq!(nodes["plain__text"]["text"], "Ocean");
}

#[test]
fn zoom_curve_text_size_routes_to_size_expr() {
    const STYLE: &str = r##"{
      "version": 8,
      "name": "dd-size",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "places", "type": "symbol", "source": "s", "source-layer": "place",
          "layout": {
            "text-field": "{name}",
            "text-font": ["F"],
            "text-size": ["interpolate", ["linear"], ["zoom"], 8, 10, 16, 22]
          } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[("F", "https://fonts.example/F.ttf")]);
    let (recipe, _) = convert(&style, &opts).unwrap();

    let text = &recipe["nodes"]["places__text"];
    assert_eq!(
        text["size-expr"],
        serde_json::json!(["interpolate", ["linear"], ["zoom"], 8, 10, 16, 22])
    );
    assert!(text.get("size").is_none(), "no constant size: {text}");
}

#[test]
fn unmapped_font_warns_and_skips_text() {
    const STYLE: &str = r##"{
      "version": 8,
      "name": "no-fonts",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "places", "type": "symbol", "source": "s", "source-layer": "place",
          "layout": { "text-field": "{name}", "text-font": ["Unmapped Sans"] } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, report) = convert(&style, &ConvertOptions::default()).unwrap();

    let nodes = recipe["nodes"].as_object().unwrap();
    assert!(
        !nodes.values().any(|n| n["op"] == "text"),
        "no text node without a font mapping"
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("no font mapping") && w.contains("Unmapped Sans")),
        "expected a font-mapping warning: {:?}",
        report.warnings
    );
}

#[test]
fn unmapped_font_with_glyphs_endpoint_emits_a_glyphs_source() {
    const STYLE: &str = r##"{
      "version": 8,
      "name": "zero-config",
      "glyphs": "https://fonts.example/{fontstack}/{range}.pbf",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "places", "type": "symbol", "source": "s", "source-layer": "place",
          "layout": { "text-field": "{name}",
                      "text-font": ["Noto Sans Regular", "Arial Unicode MS Regular"] } },
        { "id": "pois", "type": "symbol", "source": "s", "source-layer": "poi",
          "layout": { "text-field": "{name}",
                      "text-font": ["Noto Sans Regular", "Arial Unicode MS Regular"] } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, report) = convert(&style, &ConvertOptions::default()).unwrap();

    // One glyphs source per distinct stack: entries joined ", ".
    let sources = recipe["sources"].as_object().unwrap();
    let (id, glyphs) = sources
        .iter()
        .find(|(_, s)| s["type"] == "glyphs")
        .expect("a glyphs source");
    assert_eq!(
        glyphs["url"],
        "https://fonts.example/{fontstack}/{range}.pbf"
    );
    assert_eq!(
        glyphs["fontstack"],
        "Noto Sans Regular, Arial Unicode MS Regular"
    );
    assert_eq!(
        sources.values().filter(|s| s["type"] == "glyphs").count(),
        1,
        "identical stacks share one glyphs source"
    );

    // Both text nodes reference it; no skipped-text warning.
    let nodes = recipe["nodes"].as_object().unwrap();
    for layer in ["places__text", "pois__text"] {
        assert_eq!(nodes[layer]["font"], serde_json::json!([id]));
    }
    assert!(
        !report.warnings.iter().any(|w| w.contains("text skipped")),
        "unexpected warnings: {:?}",
        report.warnings
    );

    // Valid ezu Document.
    let doc_text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&doc_text).expect("recipe parses as ezu Document");
}

#[test]
fn explicit_font_mapping_wins_over_the_glyphs_endpoint() {
    const STYLE: &str = r##"{
      "version": 8,
      "name": "mapped",
      "glyphs": "https://fonts.example/{fontstack}/{range}.pbf",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "places", "type": "symbol", "source": "s", "source-layer": "place",
          "layout": { "text-field": "{name}", "text-font": ["Noto Sans Regular"] } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[("Noto Sans Regular", "https://fonts.example/NotoSans.ttf")]);
    let (recipe, _) = convert(&style, &opts).unwrap();

    let sources = recipe["sources"].as_object().unwrap();
    assert_eq!(sources["noto-sans-regular"]["type"], "font");
    assert!(
        !sources.values().any(|s| s["type"] == "glyphs"),
        "a mapped stack must not fall back to the glyphs endpoint"
    );
    assert_eq!(
        recipe["nodes"]["places__text"]["font"],
        serde_json::json!(["noto-sans-regular"])
    );
}

#[test]
fn icon_and_text_layer_emits_both_nodes() {
    const STYLE: &str = r##"{
      "version": 8,
      "name": "icon-text",
      "sprite": "https://example.com/sprites/basemap",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "pois", "type": "symbol", "source": "s", "source-layer": "poi",
          "layout": {
            "icon-image": "airport-15",
            "text-field": "{name}",
            "text-font": ["F"],
            "text-anchor": "top",
            "text-offset": [0, 1]
          } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[("F", "https://fonts.example/F.ttf")]);
    let (recipe, report) = convert(&style, &opts).unwrap();

    let nodes = recipe["nodes"].as_object().unwrap();
    let stamp = &nodes["pois__stamp"];
    let text = &nodes["pois__text"];
    assert_eq!(stamp["op"], "stamp");
    assert_eq!(text["op"], "text");
    // Both halves share the layer's features node.
    assert_eq!(stamp["features"], text["features"]);
    // Painter's algorithm: text blends over the icon.
    let blend = nodes
        .values()
        .find(|n| n["op"] == "blend")
        .expect("a blend node");
    assert!(blend["base"].as_str().unwrap().contains("__stamp"));
    assert!(blend["over"].as_str().unwrap().contains("__text"));
    // The old "text not supported" warning is gone.
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.contains("not supported yet")),
        "unexpected warnings: {:?}",
        report.warnings
    );
}

#[test]
fn font_sources_dedupe_by_url_across_layers() {
    const STYLE: &str = r##"{
      "version": 8,
      "name": "dedupe",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "a", "type": "symbol", "source": "s", "source-layer": "place",
          "layout": { "text-field": "{name}", "text-font": ["F"] } },
        { "id": "b", "type": "symbol", "source": "s", "source-layer": "poi",
          "layout": { "text-field": "{name}", "text-font": ["F"] } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[("F", "https://fonts.example/F.ttf")]);
    let (recipe, _) = convert(&style, &opts).unwrap();

    let font_sources = recipe["sources"]
        .as_object()
        .unwrap()
        .values()
        .filter(|s| s["type"] == "font")
        .count();
    assert_eq!(font_sources, 1, "one font source per distinct URL");
}
