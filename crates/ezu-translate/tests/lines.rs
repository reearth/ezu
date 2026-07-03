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
    let (recipe, _) = convert(
        &style,
        &ConvertOptions {
            zoom: Some(6.0),
            ..Default::default()
        },
    )
    .unwrap();
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
}
