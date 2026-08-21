//! `color-ramp` over a `ScalarField`. With no DEM asset bound, the
//! `dem` source node falls back to a zero-filled field — convenient
//! for testing the stop-table mapping without spinning up a fake
//! AssetLoader.

use crate::common::{render, render_with_features};

#[test]
fn color_ramp_clamps_zero_field_to_first_stop() {
    // Unbound `dem` source emits an all-zero `ScalarField`. With the
    // first stop at value 0 (red) and a later stop at 100 (blue),
    // every pixel should map exactly to the first stop's colour.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "terrain": { "type": "dem",
                     "url": "http://example.invalid/{z}/{x}/{y}.webp",
                     "encoding": "terrarium" }
      },
      "nodes": {
        "dem":  { "op": "dem", "name": "tile.terrain" },
        "out":  { "op": "color-ramp", "field": "@dem",
                  "stops": [ { "value": 0,   "color": "#ff0000" },
                             { "value": 100, "color": "#0000ff" } ] }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    assert_eq!(p, [0xff, 0x00, 0x00, 0xff], "got {p:?}");
}

#[test]
fn color_ramp_below_range_clamps_to_first_stop() {
    // Zero field with stops at 100 (red) and 1000 (blue): 0 is below
    // the lowest stop, so every pixel clamps to the first colour.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "terrain": { "type": "dem",
                     "url": "http://example.invalid/{z}/{x}/{y}.webp",
                     "encoding": "terrarium" }
      },
      "nodes": {
        "dem":  { "op": "dem", "name": "tile.terrain" },
        "out":  { "op": "color-ramp", "field": "@dem",
                  "stops": [ { "value": 100,  "color": "#ff0000" },
                             { "value": 1000, "color": "#0000ff" } ] }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    assert_eq!(p, [0xff, 0x00, 0x00, 0xff], "got {p:?}");
}

// --- `ramp-expr`: a MapLibre color expression over `heatmap-density` -------

#[test]
fn ramp_expr_lut_matches_direct_expression_evaluation() {
    // Remap the unbound-DEM zero field to a constant 0.6, then ramp it
    // with a color expression. Every pixel must match evaluating the
    // expression directly at `heatmap-density` 0.6 (the 256-entry LUT
    // has an exact entry at 0.6·255 = 153, so no interpolation slack).
    let expr = serde_json::json!([
        "interpolate",
        ["linear"],
        ["heatmap-density"],
        0,
        "rgb(10, 200, 30)",
        1,
        "rgb(250, 40, 90)"
    ]);
    let json = format!(
        r##"{{
          "name": "ramp-expr",
          "tile-size": 8,
          "sources": {{
            "terrain": {{ "type": "dem",
                          "url": "http://example.invalid/{{z}}/{{x}}/{{y}}.webp",
                          "encoding": "terrarium" }}
          }},
          "nodes": {{
            "dem":  {{ "op": "dem", "name": "tile.terrain" }},
            "mid":  {{ "op": "map-range", "field": "@dem", "out-min": 0.6, "out-max": 1.0 }},
            "out":  {{ "op": "color-ramp", "field": "@mid", "ramp-expr": {expr} }}
          }},
          "output": "@out"
        }}"##
    );
    let r = render(&json, 8, 0);
    let got = r.pixel(4, 4);

    // Direct evaluation at the same density (zoom 0, like the render).
    let parsed = maplibre_expr::parse(&expr).unwrap();
    let parsed =
        maplibre_expr::typecheck(&parsed, Some(&maplibre_expr::Type::Color), false).unwrap();
    let mut ectx = maplibre_expr::EvaluationContext::new().with_zoom(0.0);
    ectx.heatmap_density = Some(0.6);
    let maplibre_expr::Value::Color(c) = maplibre_expr::evaluate(&parsed, &ectx).unwrap() else {
        panic!("expected a color");
    };
    let want = [
        (c.r * 255.0).round() as u8,
        (c.g * 255.0).round() as u8,
        (c.b * 255.0).round() as u8,
        (c.a * 255.0).round() as u8,
    ];
    for ch in 0..4 {
        assert!(
            (got[ch] as i32 - want[ch] as i32).abs() <= 1,
            "channel {ch}: got {got:?}, want {want:?}"
        );
    }
}

#[test]
fn ramp_expr_paints_the_hot_color_at_a_density_peak() {
    // One point through `density` and the MapLibre spec's default
    // `heatmap-color` ramp: red-hot at the peak (density clamps to 1),
    // opaque mid-ramp colors around it, transparent beyond the kernel.
    use ezu_features::{Feature, FeatureLayer, Geometry};
    use ezu_graph::TileId;

    let mut geometry = Geometry::default();
    geometry.points.push((2048, 2048)); // canvas px (16, 16) on a 32px tile
    let layer = FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features: vec![Feature {
            id: None,
            geometry,
            properties: std::collections::HashMap::new(),
        }],
    };

    let json = r##"{
      "name": "ramp-expr-peak",
      "tile-size": 32,
      "sources": {
        "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" }
      },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "pts" },
        "dens":  { "op": "density", "features": "@feats", "radius": 10, "intensity": 3 },
        "out":   { "op": "color-ramp", "field": "@dens",
                   "ramp-expr": ["interpolate", ["linear"], ["heatmap-density"],
                                 0, "rgba(0, 0, 255, 0)",
                                 0.1, "#4169e1",
                                 0.3, "cyan",
                                 0.5, "lime",
                                 0.7, "yellow",
                                 1, "red"] }
      },
      "output": "@out"
    }"##;
    let r = render_with_features(
        json,
        32,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer)],
    );

    // Peak density ≈ 3·0.39 clamps to 1 → the ramp's hot end (red).
    let center = r.pixel(16, 16);
    assert!(
        center[0] > 220 && center[1] < 60 && center[3] > 220,
        "center should be hot red, got {center:?}"
    );
    // Part-way out the density is mid-ramp: opaque and not red.
    let mid = r.pixel(21, 16);
    assert!(
        mid[3] == 255 && mid[1] > center[1],
        "mid-kernel should fade through the ramp, got {mid:?}"
    );
    // Beyond the kernel: density 0 → rgba(0,0,255,0) → fully transparent.
    assert_eq!(r.pixel(28, 16), [0, 0, 0, 0]);
}

#[test]
fn stops_still_required_without_ramp_expr() {
    use ezu_graph::build_graph;
    use ezu_paint::nodes::default_registry;
    use ezu_style::Document;

    let json = r##"{
      "name": "no-stops",
      "tile-size": 8,
      "sources": {
        "terrain": { "type": "dem",
                     "url": "http://example.invalid/{z}/{x}/{y}.webp",
                     "encoding": "terrarium" }
      },
      "nodes": {
        "dem": { "op": "dem", "name": "tile.terrain" },
        "out": { "op": "color-ramp", "field": "@dem" }
      },
      "output": "@out"
    }"##;
    let doc = Document::from_json(json).expect("parse");
    assert!(
        build_graph(&doc, &default_registry()).is_err(),
        "color-ramp without stops or ramp-expr must fail at build"
    );
}

// --- `opacity`: uniform output-alpha multiplier ----------------------------

#[test]
fn constant_opacity_scales_output_alpha() {
    // Zero field on the red stop with opacity 0.5: premultiplied
    // [128, 0, 0, 128] instead of solid red.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "terrain": { "type": "dem",
                     "url": "http://example.invalid/{z}/{x}/{y}.webp",
                     "encoding": "terrarium" }
      },
      "nodes": {
        "dem":  { "op": "dem", "name": "tile.terrain" },
        "out":  { "op": "color-ramp", "field": "@dem", "opacity": 0.5,
                  "stops": [ { "value": 0,   "color": "#ff0000" },
                             { "value": 100, "color": "#0000ff" } ] }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    assert_eq!(p, [0x80, 0x00, 0x00, 0x80], "got {p:?}");
}

#[test]
fn expr_node_drives_opacity_as_a_zoom_curve() {
    // The heatmap→circle crossfade shape: an `expr` scalar node holding a
    // zoom curve, wired into the ramp's `opacity` port. Fully opaque at
    // z0, fully transparent at z4.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "terrain": { "type": "dem",
                     "url": "http://example.invalid/{z}/{x}/{y}.webp",
                     "encoding": "terrarium" }
      },
      "nodes": {
        "dem":  { "op": "dem", "name": "tile.terrain" },
        "fade": { "op": "expr",
                  "expr": ["interpolate", ["linear"], ["zoom"], 0, 1, 4, 0] },
        "out":  { "op": "color-ramp", "field": "@dem", "opacity": "@fade",
                  "stops": [ { "value": 0,   "color": "#ff0000" },
                             { "value": 100, "color": "#0000ff" } ] }
      },
      "output": "@out"
    }"##;
    use crate::common::render_tile;
    use ezu_graph::TileId;
    let opaque = render(json, 8, 0);
    assert_eq!(opaque.pixel(4, 4), [0xff, 0x00, 0x00, 0xff]);
    let faded = render_tile(json, 8, 0, TileId { z: 4, x: 0, y: 0 });
    assert_eq!(faded.pixel(4, 4), [0x00, 0x00, 0x00, 0x00]);
}
