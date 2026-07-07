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
fn collision_properties_route_to_the_text_node() {
    const STYLE: &str = r##"{
      "version": 8,
      "name": "collision",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "places", "type": "symbol", "source": "s", "source-layer": "place",
          "filter": ["==", ["get", "class"], "city"],
          "layout": {
            "text-field": "{name}",
            "text-font": ["F"],
            "text-allow-overlap": true,
            "text-ignore-placement": true,
            "text-padding": 4,
            "symbol-sort-key": ["get", "rank"]
          } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[("F", "https://fonts.example/F.ttf")]);
    let (recipe, _) = convert(&style, &opts).unwrap();

    let text = &recipe["nodes"]["places__text"];
    // Neighbour-gathering wiring: origin source/layer + the layer filter,
    // reproduced so neighbour candidates filter identically.
    assert_eq!(text["source"], "s");
    assert_eq!(text["layer"], "place");
    assert_eq!(
        text["filter-expr"],
        serde_json::json!(["==", ["get", "class"], "city"])
    );
    // Overlap knobs.
    assert_eq!(text["allow-overlap"], true);
    assert_eq!(text["ignore-placement"], true);
    assert_eq!(text["padding-px"], 4.0);
    assert_eq!(text["sort-key-expr"], serde_json::json!(["get", "rank"]));

    // Still a valid ezu Document.
    let doc_text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&doc_text).expect("recipe parses as ezu Document");
}

#[test]
fn expression_text_padding_routes_to_padding_expr() {
    // A zoom-curve `text-padding` becomes the text node's `padding-expr`
    // (evaluated per feature), not a dropped constant.
    const STYLE: &str = r##"{
      "version": 8,
      "name": "dd-padding",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "places", "type": "symbol", "source": "s", "source-layer": "place",
          "layout": {
            "text-field": "{name}",
            "text-font": ["F"],
            "text-padding": ["interpolate", ["linear"], ["zoom"], 6, 2, 16, 12]
          } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[("F", "https://fonts.example/F.ttf")]);
    let (recipe, report) = convert(&style, &opts).unwrap();

    let text = &recipe["nodes"]["places__text"];
    assert_eq!(
        text["padding-expr"],
        serde_json::json!(["interpolate", ["linear"], ["zoom"], 6, 2, 16, 12])
    );
    assert!(
        text.get("padding-px").is_none(),
        "no constant padding: {text}"
    );
    assert!(
        !report.warnings.iter().any(|w| w.contains("text-padding")),
        "data-driven text-padding should not warn: {:?}",
        report.warnings
    );

    let doc_text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&doc_text).expect("recipe parses as ezu Document");
}

#[test]
fn text_variable_anchor_routes_to_anchor_variants() {
    // A `text-variable-anchor` list + `text-radial-offset` become the text
    // node's `anchor-variants` / `radial-offset`, with no unsupported warning.
    const STYLE: &str = r##"{
      "version": 8,
      "name": "variable-anchor",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "pois", "type": "symbol", "source": "s", "source-layer": "poi",
          "layout": {
            "text-field": "{name}",
            "text-font": ["F"],
            "text-anchor": "center",
            "text-variable-anchor": ["top", "bottom", "left", "right"],
            "text-radial-offset": 1.2
          } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[("F", "https://fonts.example/F.ttf")]);
    let (recipe, report) = convert(&style, &opts).unwrap();

    let text = &recipe["nodes"]["pois__text"];
    assert_eq!(
        text["anchor-variants"],
        serde_json::json!(["top", "bottom", "left", "right"])
    );
    assert_eq!(text["radial-offset"], 1.2);
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.contains("text-variable-anchor")),
        "variable-anchor should not warn: {:?}",
        report.warnings
    );

    let doc_text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&doc_text).expect("recipe parses as ezu Document");
}

#[test]
fn expression_text_variable_anchor_warns_and_falls_back() {
    // A data-driven `text-variable-anchor` (not a plain string array) can't be
    // resolved at build time, so it warns and no `anchor-variants` is emitted.
    const STYLE: &str = r##"{
      "version": 8,
      "name": "variable-anchor-expr",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "pois", "type": "symbol", "source": "s", "source-layer": "poi",
          "layout": {
            "text-field": "{name}",
            "text-font": ["F"],
            "text-variable-anchor": ["case", ["get", "big"], ["literal", ["top"]], ["literal", ["bottom"]]]
          } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[("F", "https://fonts.example/F.ttf")]);
    let (recipe, report) = convert(&style, &opts).unwrap();

    let text = &recipe["nodes"]["pois__text"];
    assert!(
        text.get("anchor-variants").is_none(),
        "an expression variable-anchor should not emit anchor-variants: {text}"
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("text-variable-anchor")),
        "expected a variable-anchor warning: {:?}",
        report.warnings
    );
}

#[test]
fn text_overlap_enum_maps_and_cooperative_warns() {
    const STYLE: &str = r##"{
      "version": 8,
      "name": "overlap-enum",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "always", "type": "symbol", "source": "s", "source-layer": "a",
          "layout": { "text-field": "{name}", "text-font": ["F"], "text-overlap": "always" } },
        { "id": "never", "type": "symbol", "source": "s", "source-layer": "b",
          "layout": { "text-field": "{name}", "text-font": ["F"], "text-overlap": "never" } },
        { "id": "coop", "type": "symbol", "source": "s", "source-layer": "c",
          "layout": { "text-field": "{name}", "text-font": ["F"], "text-overlap": "cooperative" } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[("F", "https://fonts.example/F.ttf")]);
    let (recipe, report) = convert(&style, &opts).unwrap();

    let nodes = recipe["nodes"].as_object().unwrap();
    // `always` → allow-overlap true.
    assert_eq!(nodes["always__text"]["allow-overlap"], true);
    // `never` → collide (no allow-overlap field emitted).
    assert!(nodes["never__text"].get("allow-overlap").is_none());
    // `cooperative` → treated as never (collide) with a warning.
    assert!(nodes["coop__text"].get("allow-overlap").is_none());
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("cooperative") && w.contains("coop")),
        "expected a cooperative-overlap warning: {:?}",
        report.warnings
    );
}

#[test]
fn line_placement_routes_to_the_text_node() {
    const STYLE: &str = r##"{
      "version": 8,
      "name": "line-placement",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "streets", "type": "symbol", "source": "s", "source-layer": "street",
          "layout": {
            "text-field": "{name}",
            "text-font": ["F"],
            "symbol-placement": "line",
            "symbol-spacing": 400,
            "text-max-angle": 30,
            "text-keep-upright": false
          } },
        { "id": "rivers", "type": "symbol", "source": "s", "source-layer": "water",
          "layout": {
            "text-field": "{name}",
            "text-font": ["F"],
            "symbol-placement": "line-center"
          } },
        { "id": "viewport", "type": "symbol", "source": "s", "source-layer": "rail",
          "layout": {
            "text-field": "{name}",
            "text-font": ["F"],
            "symbol-placement": "line",
            "text-rotation-alignment": "viewport"
          } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[("F", "https://fonts.example/F.ttf")]);
    let (recipe, report) = convert(&style, &opts).unwrap();

    let nodes = recipe["nodes"].as_object().unwrap();
    // Full knob routing.
    let streets = &nodes["streets__text"];
    assert_eq!(streets["placement"], "line");
    assert_eq!(streets["spacing-px"], 400.0);
    assert_eq!(streets["max-angle-deg"], 30.0);
    assert_eq!(streets["keep-upright"], false);
    // Defaults stay off the node (the `text` node's own defaults apply).
    let rivers = &nodes["rivers__text"];
    assert_eq!(rivers["placement"], "line-center");
    assert!(rivers.get("spacing-px").is_none());
    assert!(rivers.get("keep-upright").is_none());
    // No line-placement skip warning; viewport rotation alignment warns
    // but the text still converts.
    assert!(
        !report.warnings.iter().any(|w| w.contains("text skipped")),
        "unexpected warnings: {:?}",
        report.warnings
    );
    assert_eq!(nodes["viewport__text"]["placement"], "line");
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("text-rotation-alignment: viewport")),
        "expected a rotation-alignment warning: {:?}",
        report.warnings
    );

    // Still a valid ezu Document.
    let doc_text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&doc_text).expect("recipe parses as ezu Document");
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

#[test]
fn legacy_stops_text_field_expands_tokens_into_a_step_expression() {
    // demotiles-style `text-field`: a legacy zoom-interval function whose
    // stop outputs carry `{token}`s. Raw passthrough would render the
    // token text literally; it must lower to `step` with expanded outputs.
    const STYLE: &str = r##"{
      "version": 8,
      "name": "legacy-tokens",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "countries", "type": "symbol", "source": "s", "source-layer": "place",
          "layout": {
            "text-field": { "stops": [[2, "{ABBREV}"], [4, "{NAME}"]] },
            "text-font": ["F"]
          } },
        { "id": "plainstops", "type": "symbol", "source": "s", "source-layer": "sea",
          "layout": {
            "text-field": { "stops": [[2, "Sea"], [4, "Ocean"]] },
            "text-font": ["F"]
          } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[("F", "https://fonts.example/F.ttf")]);
    let (recipe, _) = convert(&style, &opts).unwrap();

    let nodes = recipe["nodes"].as_object().unwrap();
    assert_eq!(
        nodes["countries__text"]["text"],
        serde_json::json!([
            "step",
            ["zoom"],
            ["to-string", ["get", "ABBREV"]],
            4.0,
            ["to-string", ["get", "NAME"]]
        ])
    );
    // Token-free legacy stops keep the raw passthrough.
    assert!(nodes["plainstops__text"]["text"].is_object());
}

#[test]
fn data_driven_text_font_emits_font_expr_and_registry() {
    // A `case` over two `["literal", [...]]` stacks (MapLibre data-driven
    // `text-font`). Both stacks map via `--font`, so the node emits `font-expr`
    // (raw), a `font-stacks` registry keyed by the canonical `,`-joined names,
    // and `font` = the first enumerated stack.
    const STYLE: &str = r##"{
      "version": 8,
      "name": "ddf",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "places", "type": "symbol", "source": "s", "source-layer": "place",
          "layout": {
            "text-field": ["get", "name"],
            "text-font": ["case", ["==", ["get", "script"], "Devanagari"],
                          ["literal", ["Noto Sans Devanagari Regular"]],
                          ["literal", ["Noto Sans Regular"]]]
          } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[
        ("Noto Sans Regular", "https://fonts.example/NotoSans.ttf"),
        (
            "Noto Sans Devanagari Regular",
            "https://fonts.example/NotoSansDevanagari.ttf",
        ),
    ]);
    let (recipe, report) = convert(&style, &opts).unwrap();

    let nodes = recipe["nodes"].as_object().unwrap();
    let text = nodes
        .values()
        .find(|n| n["op"] == "text")
        .expect("text node");

    // `font` is the first enumerated stack (document order → the Devanagari
    // branch appears first in the `case`).
    assert_eq!(
        text["font"],
        serde_json::json!(["noto-sans-devanagari-regular"])
    );
    // `font-expr` is the raw MapLibre expression, passed through verbatim.
    assert_eq!(
        text["font-expr"],
        serde_json::json!([
            "case",
            ["==", ["get", "script"], "Devanagari"],
            ["literal", ["Noto Sans Devanagari Regular"]],
            ["literal", ["Noto Sans Regular"]]
        ])
    );
    // Registry keyed by canonical `,`-joined stack names.
    let stacks = text["font-stacks"].as_object().expect("font-stacks object");
    assert_eq!(
        stacks["Noto Sans Devanagari Regular"],
        serde_json::json!(["noto-sans-devanagari-regular"])
    );
    assert_eq!(
        stacks["Noto Sans Regular"],
        serde_json::json!(["noto-sans-regular"])
    );

    // Both fonts declared as sources; no "text skipped" warning.
    let sources = recipe["sources"].as_object().unwrap();
    assert_eq!(sources["noto-sans-regular"]["type"], "font");
    assert_eq!(sources["noto-sans-devanagari-regular"]["type"], "font");
    assert!(
        !report.warnings.iter().any(|w| w.contains("text skipped")),
        "unexpected warnings: {:?}",
        report.warnings
    );

    let doc_text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&doc_text).expect("recipe parses as ezu Document");
}

#[test]
fn data_driven_text_font_without_literals_falls_back_to_default() {
    // A `text-font` expression with no enumerable literal stacks: no
    // `font-expr` is emitted, the default stack is used, and the layer's text
    // is *not* skipped (renders more of the map than the old behaviour).
    const STYLE: &str = r##"{
      "version": 8,
      "name": "ddf-nolit",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "places", "type": "symbol", "source": "s", "source-layer": "place",
          "layout": {
            "text-field": ["get", "name"],
            "text-font": ["coalesce", ["get", "font_a"], ["get", "font_b"]]
          } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[("Open Sans Regular", "https://fonts.example/OpenSans.ttf")]);
    let (recipe, report) = convert(&style, &opts).unwrap();

    let nodes = recipe["nodes"].as_object().unwrap();
    let text = nodes
        .values()
        .find(|n| n["op"] == "text")
        .expect("text node");
    assert!(
        text.get("font-expr").is_none(),
        "no font-expr for unenumerable text-font"
    );
    assert!(text.get("font-stacks").is_none());
    assert!(text["font"].is_array());
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("no literal stacks found")),
        "expected a no-literal-stacks warning: {:?}",
        report.warnings
    );
}

#[test]
fn all_string_array_expression_text_font_is_treated_as_dynamic() {
    // `["get", "x"]` is syntactically an all-string array but semantically an
    // expression. It must NOT be mistaken for a static stack named `get`/`x`
    // (which maps to nothing and would skip the layer's text); it takes the
    // data-driven path, finds no literal stacks, and falls back to the default
    // stack — the layer's text still renders.
    const STYLE: &str = r##"{
      "version": 8,
      "name": "ddf-getexpr",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "places", "type": "symbol", "source": "s", "source-layer": "place",
          "layout": {
            "text-field": ["get", "name"],
            "text-font": ["get", "font_prop"]
          } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[("Open Sans Regular", "https://fonts.example/OpenSans.ttf")]);
    let (recipe, report) = convert(&style, &opts).unwrap();

    let nodes = recipe["nodes"].as_object().unwrap();
    let text = nodes
        .values()
        .find(|n| n["op"] == "text")
        .expect("text node");
    assert!(
        text.get("font-expr").is_none(),
        "no enumerable stacks → no font-expr"
    );
    assert!(text["font"].is_array(), "falls back to the default stack");
    assert!(
        !report.warnings.iter().any(|w| w.contains("text skipped")),
        "the layer's text must not be skipped: {:?}",
        report.warnings
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("no literal stacks found")),
        "expected a no-literal-stacks warning: {:?}",
        report.warnings
    );
}

#[test]
fn format_text_field_passes_through_and_registers_section_fonts() {
    // A `format` label with a per-section `text-font` (and a `vertical-align`
    // to warn about). The expression passes through verbatim on `text`, and
    // the section's stack is registered in `font-stacks`.
    const STYLE: &str = r##"{
      "version": 8,
      "name": "fmt",
      "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
      "layers": [
        { "id": "places", "type": "symbol", "source": "s", "source-layer": "place",
          "layout": {
            "text-font": ["Noto Sans Regular"],
            "text-field": ["format",
              ["get", "name"], {},
              "\n", {},
              ["get", "name:hi"], {"text-font": ["literal", ["Noto Sans Devanagari Regular"]],
                                   "font-scale": 0.8, "vertical-align": "bottom"}]
          } }
      ]
    }"##;
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let opts = opts_with_fonts(&[
        ("Noto Sans Regular", "https://fonts.example/NotoSans.ttf"),
        (
            "Noto Sans Devanagari Regular",
            "https://fonts.example/NotoSansDevanagari.ttf",
        ),
    ]);
    let (recipe, report) = convert(&style, &opts).unwrap();
    let nodes = recipe["nodes"].as_object().unwrap();
    let text = nodes
        .values()
        .find(|n| n["op"] == "text")
        .expect("text node");

    // The `format` expression passes through unchanged (no flatten to concat).
    assert_eq!(text["text"][0], "format");
    // The section's `text-font` is registered under its canonical key.
    assert_eq!(
        text["font-stacks"]["Noto Sans Devanagari Regular"],
        serde_json::json!(["noto-sans-devanagari-regular"])
    );
    // `vertical-align` is rendered natively now, so it no longer warns, and
    // `format` is not "dropped" / flattened.
    assert!(
        !report.warnings.iter().any(|w| w.contains("vertical-align")),
        "vertical-align should render without a warning: {:?}",
        report.warnings
    );
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.contains("format") && w.contains("dropped")),
        "format must not be flattened: {:?}",
        report.warnings
    );

    let doc_text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&doc_text).expect("recipe parses as ezu Document");
}
