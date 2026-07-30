//! Lines convert to the crisp `stroke` op (with dash/cap/join), and
//! `fill-outline-color` becomes a fill-solid `edge`.

use ezu_translate::maplibre::{convert, ConvertOptions};

const STYLE: &str = r##"{
  "version": 8,
  "name": "lines",
  "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
  "layers": [
    { "id": "land", "type": "fill", "source": "s", "source-layer": "earth",
      "paint": { "fill-color": "#eeeeee", "fill-outline-color": "#333333" } },
    { "id": "border", "type": "line", "source": "s", "source-layer": "admin",
      "layout": { "line-cap": "round", "line-join": "round" },
      "paint": { "line-color": "#808080", "line-width": 2, "line-dasharray": [3, 2] } }
  ]
}"##;

#[test]
fn lines_use_stroke_with_dash_and_fill_has_outline() {
    let style: serde_json::Value = serde_json::from_str(STYLE).unwrap();
    let (recipe, _) = convert(&style, &ConvertOptions::default()).unwrap();
    let nodes = recipe["nodes"].as_object().unwrap();

    // Line → crisp `stroke` with cap/join and pixel dash (3,2 × width 2 = 6,4).
    let stroke = nodes
        .values()
        .find(|n| n["op"] == "stroke")
        .expect("a stroke node");
    assert_eq!(stroke["width-px"], 2.0);
    assert_eq!(stroke["cap"], "round");
    assert_eq!(stroke["join"], "round");
    assert_eq!(stroke["dasharray"], serde_json::json!([6.0, 4.0]));
    // No painterly brush nodes anymore.
    assert!(!nodes
        .values()
        .any(|n| n["op"] == "line" || n["op"] == "brush-solid"));

    // fill-outline-color → fill-solid edge.
    let fill = nodes
        .values()
        .find(|n| n["op"] == "fill-solid")
        .expect("a fill-solid node");
    assert_eq!(fill["edge"], "#333333");

    // Valid Document.
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
    // Without `line-gap-width` the stroke stays a plain centreline stroke.
    assert!(stroke.get("gap-width-px").is_none());
}

const CASING_STYLE: &str = r##"{
  "version": 8,
  "name": "casings",
  "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
  "layers": [
    { "id": "road_casing", "type": "line", "source": "s", "source-layer": "roads",
      "paint": { "line-color": "#ffffff", "line-width": 1.5, "line-gap-width": 6 } },
    { "id": "road_casing_dd", "type": "line", "source": "s", "source-layer": "roads",
      "paint": { "line-color": "#ffffff", "line-width": 1.5,
                 "line-gap-width": ["interpolate", ["linear"], ["zoom"], 12, 2, 16, 10] } }
  ]
}"##;

#[test]
fn line_gap_width_becomes_a_stroke_casing() {
    let style: serde_json::Value = serde_json::from_str(CASING_STYLE).unwrap();
    let (recipe, report) = convert(&style, &ConvertOptions::default()).unwrap();
    let nodes = recipe["nodes"].as_object().unwrap();

    let constant = &nodes["road_casing__stroke"];
    assert_eq!(constant["gap-width-px"], 6.0);
    assert!(constant.get("gap-width-expr").is_none());

    let data_driven = &nodes["road_casing_dd__stroke"];
    assert_eq!(
        data_driven["gap-width-expr"],
        serde_json::json!(["interpolate", ["linear"], ["zoom"], 12, 2, 16, 10])
    );
    assert!(data_driven.get("gap-width-px").is_none());

    assert!(
        !report.warnings.iter().any(|w| w.contains("gap")),
        "gap widths are supported, not warned about: {:?}",
        report.warnings
    );

    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}
