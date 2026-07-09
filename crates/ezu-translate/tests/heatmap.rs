//! A MapLibre `heatmap` layer lowers to `features` → `density` →
//! `color-ramp`, with constant-vs-expression paint routing and the
//! layer's `heatmap-color` passed through raw as `ramp-expr`.

use ezu_translate::maplibre::{convert, ConvertOptions};
use serde_json::Value;

/// The only warning we expect from an otherwise-clean heatmap conversion is
/// the pad lift (the default radius exceeds the default pad). Filter it out so
/// tests can still assert nothing *else* was flagged.
fn warnings_besides_pad(report: &ezu_translate::maplibre::Report) -> Vec<&String> {
    report
        .warnings
        .iter()
        .filter(|w| !w.contains("raised the recipe pad"))
        .collect()
}

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
        warnings_besides_pad(&report).is_empty(),
        "unexpected warnings: {:?}",
        report.warnings
    );

    let feat = node(&recipe, "features");
    assert_eq!(feat["source"], "quakes");
    assert_eq!(feat["layer"], "events");

    // Expression radius: raw expr on `radius-expr`, and the constant pad
    // bound derived from the expression's max literal output stop (20). The
    // document pad is lifted from the default 16 to cover that 20px kernel.
    assert_eq!(recipe["pad"], 20);
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
        warnings_besides_pad(&report).is_empty(),
        "unexpected warnings: {:?}",
        report.warnings
    );

    // No `heatmap-radius`: the node keeps its 30px default, and the document
    // pad is lifted from 16 to 30 to cover that default kernel.
    assert_eq!(recipe["pad"], 30);
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
    assert!(
        warnings_besides_pad(&report).is_empty(),
        "{:?}",
        report.warnings
    );

    // Constant 25px radius > default pad 16 → pad lifted to 25.
    assert_eq!(recipe["pad"], 25);
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
    // The 100px capped bound also drives the pad lift.
    assert_eq!(recipe["pad"], 100);
}

#[test]
fn a_user_pad_above_the_requirement_is_kept_as_a_floor() {
    // The requested pad is a floor, not an override: a generous 100 stays
    // put when the 30px default kernel only needs 30, and no lift is warned.
    let style = style_with_paint("{}");
    let opts = ConvertOptions {
        pad: 100,
        ..ConvertOptions::default()
    };
    let (recipe, report) = convert(&style, &opts).expect("convert");
    assert_eq!(recipe["pad"], 100);
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.contains("raised the recipe pad")),
        "no lift expected, got {:?}",
        report.warnings
    );
}

#[test]
fn a_user_pad_below_the_requirement_is_lifted() {
    // A requested pad smaller than the kernel needs is lifted to cover it.
    let style = style_with_paint(r##"{ "heatmap-radius": 48 }"##);
    let opts = ConvertOptions {
        pad: 16,
        ..ConvertOptions::default()
    };
    let (recipe, report) = convert(&style, &opts).expect("convert");
    assert_eq!(recipe["pad"], 48);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("raised the recipe pad")),
        "expected a pad-lift warning, got {:?}",
        report.warnings
    );
}

#[test]
fn a_style_without_heatmap_keeps_the_requested_pad() {
    // No pad-hungry layer: the emitted pad is exactly what was requested,
    // with no lift warning.
    let style: Value = serde_json::from_str(
        r##"{
          "version": 8, "name": "plain",
          "sources": { "s": { "type": "vector", "tiles": ["https://e.com/{z}/{x}/{y}.pbf"] } },
          "layers": [
            { "id": "bg", "type": "background", "paint": { "background-color": "#fff" } },
            { "id": "w", "type": "fill", "source": "s", "source-layer": "water",
              "paint": { "fill-color": "#00f" } }
          ]
        }"##,
    )
    .unwrap();
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("convert");
    assert_eq!(recipe["pad"], 16);
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.contains("raised the recipe pad")),
        "no lift expected, got {:?}",
        report.warnings
    );
}

#[test]
fn the_emitted_pad_covers_the_density_nodes_required_pad() {
    // End-to-end structural check: build the real graph from a converted
    // heatmap recipe and confirm the emitted document pad actually covers
    // the `density` kernel — i.e. no seam-inducing clip at tile borders.
    use ezu_graph::build_graph;
    use ezu_paint::nodes::default_registry;
    use ezu_style::Document;

    for radius in [
        "30",
        "48",
        r#"["interpolate", ["linear"], ["zoom"], 0, 5, 12, 64]"#,
    ] {
        let style = style_with_paint(&format!(r#"{{ "heatmap-radius": {radius} }}"#));
        let (recipe, _) = convert(&style, &ConvertOptions::default()).expect("convert");
        let doc_pad = recipe["pad"].as_u64().expect("pad") as u32;

        let doc = Document::from_json(&serde_json::to_string(&recipe).unwrap()).expect("parse");
        let graph = build_graph(&doc, &default_registry()).expect("build");
        // `compute_pad` walks the DAG growing pad upstream; the `density`
        // node adds its radius bound over its downstream pad. Its own
        // required pad (equal to the emitted doc pad, since color-ramp and
        // stack pass pad through) must therefore not exceed what we emit.
        let pads = graph.compute_pad(doc_pad).expect("compute_pad");
        let dens_ix = (0..graph.len())
            .find(|&ix| graph.node_id(ix).ends_with("__density"))
            .expect("a density node");
        assert!(
            pads[dens_ix] <= doc_pad,
            "radius {radius}: density needs pad {}, emitted only {doc_pad}",
            pads[dens_ix]
        );
    }
}

#[test]
fn the_largest_heatmap_radius_across_layers_drives_the_pad() {
    // Two heatmap layers: the pad covers the larger kernel.
    let style: Value = serde_json::from_str(
        r##"{
          "version": 8, "name": "two",
          "sources": { "q": { "type": "vector", "tiles": ["https://e.com/{z}/{x}/{y}.pbf"] } },
          "layers": [
            { "id": "small", "type": "heatmap", "source": "q", "source-layer": "e",
              "paint": { "heatmap-radius": 20 } },
            { "id": "big", "type": "heatmap", "source": "q", "source-layer": "e",
              "paint": { "heatmap-radius": 55 } }
          ]
        }"##,
    )
    .unwrap();
    let (recipe, _) = convert(&style, &ConvertOptions::default()).expect("convert");
    assert_eq!(recipe["pad"], 55);
}

#[test]
fn constant_opacity_routes_to_the_ramp_field() {
    let style = style_with_paint(r#"{ "heatmap-opacity": 0.6 }"#);
    let (recipe, _) = convert(&style, &ConvertOptions::default()).unwrap();
    let ramp = node(&recipe, "color-ramp");
    assert_eq!(ramp["opacity"], 0.6);
}

#[test]
fn opacity_zoom_curve_becomes_an_expr_scalar_node() {
    // The canonical heatmap→circle crossfade: opacity interpolated down
    // over zoom lowers to an `expr` node wired into the ramp's port.
    let style = style_with_paint(
        r#"{ "heatmap-opacity": ["interpolate", ["linear"], ["zoom"], 13, 1, 15, 0] }"#,
    );
    let (recipe, _) = convert(&style, &ConvertOptions::default()).unwrap();
    let expr = node(&recipe, "expr");
    assert_eq!(expr["expr"][0], "interpolate");
    let ramp = node(&recipe, "color-ramp");
    assert_eq!(ramp["opacity"], "@heat__opacity");
}
