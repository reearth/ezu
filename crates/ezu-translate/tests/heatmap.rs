//! A MapLibre `heatmap` layer lowers to `features` → `density` →
//! `color-ramp`, with constant-vs-expression paint routing and the
//! layer's `heatmap-color` passed through raw as `ramp-expr`.

use ezu_translate::maplibre::{convert, ConvertOptions};
use serde_json::Value;

fn style_with_paint(paint: &str) -> Value {
    let s = format!(
        r##"{{
          "version": 8,
          "name": "hm",
          "sources": {{
            "quakes": {{ "type": "vector", "tiles": ["https://example.com/{{z}}/{{x}}/{{y}}.pbf"] }}
          }},
          "layers": [
            {{ "id": "heat", "type": "heatmap", "source": "quakes",
               "source-layer": "events", "paint": {} }}
          ]
        }}"##,
        paint
    );
    serde_json::from_str(&s).unwrap()
}

fn node<'a>(recipe: &'a Value, op: &str) -> &'a Value {
    recipe["nodes"]
        .as_object()
        .unwrap()
        .values()
        .find(|n| n["op"] == op)
        .unwrap_or_else(|| panic!("a `{op}` node"))
}

#[test]
fn heatmap_layer_lowers_to_density_and_color_ramp() {
    let style = style_with_paint(
        r##"{
          "heatmap-radius": ["interpolate", ["linear"], ["zoom"], 0, 2, 9, 20],
          "heatmap-weight": ["get", "mag"],
          "heatmap-intensity": 1.2,
          "heatmap-color": ["interpolate", ["linear"], ["heatmap-density"],
                            0, "rgba(0, 0, 255, 0)", 1, "red"]
        }"##,
    );
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("convert");
    assert!(
        report.warnings.is_empty(),
        "unexpected warnings: {:?}",
        report.warnings
    );

    let feat = node(&recipe, "features");
    assert_eq!(feat["source"], "quakes");
    assert_eq!(feat["layer"], "events");

    // Expression radius: raw expr on `radius-expr`, and the constant pad
    // bound derived from the expression's max literal output stop (20).
    let dens = node(&recipe, "density");
    assert!(dens["features"].as_str().unwrap().starts_with('@'));
    assert_eq!(dens["radius"], 20.0);
    assert_eq!(dens["radius-expr"][0], "interpolate");
    assert_eq!(dens["weight-expr"], serde_json::json!(["get", "mag"]));
    assert_eq!(dens["intensity"], 1.2);

    // `heatmap-color` passes through raw as the ramp's `ramp-expr`.
    let ramp = node(&recipe, "color-ramp");
    assert_eq!(ramp["ramp-expr"][2], serde_json::json!(["heatmap-density"]));
    assert_eq!(recipe["output"].as_str().unwrap(), "heat__ramp");

    // Still a valid ezu Document.
    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}

#[test]
fn heatmap_defaults_emit_the_spec_default_ramp() {
    // No paint at all: node defaults carry radius/weight/intensity, and
    // the spec's documented default heatmap-color becomes the ramp.
    let style = style_with_paint("{}");
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("convert");
    assert!(
        report.warnings.is_empty(),
        "unexpected warnings: {:?}",
        report.warnings
    );

    let dens = node(&recipe, "density");
    assert!(dens.get("radius").is_none(), "node default radius applies");
    assert!(dens.get("weight-expr").is_none());
    assert!(dens.get("intensity").is_none());

    let ramp = node(&recipe, "color-ramp")["ramp-expr"].to_string();
    assert!(ramp.contains("heatmap-density"), "default ramp: {ramp}");
    assert!(ramp.contains("#4169e1"), "royalblue as hex: {ramp}");
    assert!(
        ramp.contains("rgba(0, 0, 255, 0)"),
        "transparent toe: {ramp}"
    );

    let text = serde_json::to_string(&recipe).unwrap();
    ezu_style::Document::from_json(&text).expect("recipe parses as ezu Document");
}

#[test]
fn constant_radius_and_weight_route_to_constant_fields() {
    let style = style_with_paint(r##"{ "heatmap-radius": 25, "heatmap-weight": 2 }"##);
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("convert");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let dens = node(&recipe, "density");
    assert_eq!(dens["radius"], 25.0);
    assert!(dens.get("radius-expr").is_none());
    // The node has no constant weight field; a bare number is a valid
    // literal expression.
    assert_eq!(dens["weight-expr"], 2.0);
}

#[test]
fn underivable_radius_expression_falls_back_to_the_capped_bound() {
    // A data-driven radius has no literal maximum; the pad bound falls
    // back to the documented 100px cap, with a warning.
    let style = style_with_paint(r##"{ "heatmap-radius": ["get", "r"] }"##);
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("convert");
    assert!(
        report.warnings.iter().any(|w| w.contains("heatmap-radius")),
        "expected a radius-cap warning, got {:?}",
        report.warnings
    );
    let dens = node(&recipe, "density");
    assert_eq!(dens["radius"], 100.0);
    assert_eq!(dens["radius-expr"], serde_json::json!(["get", "r"]));
}
