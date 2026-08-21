//! Document-level parameters: `$param` substitution, runtime
//! overrides, min/max clamping, `@node` scalar ports (`math`, `zoom`),
//! and cache invalidation across param changes.

use crate::common::{render, render_tile, render_with_params};

use ezu_graph::{
    build_graph, Cache, CanvasInfo, Evaluator, NoAssets, ParamValues, PortValue, ScalarValue,
    TileId,
};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

const Z0: TileId = TileId { z: 0, x: 0, y: 0 };

#[test]
fn param_substitution_works() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "params": { "bg": { "type": "color", "default": "#102030" } },
      "nodes": { "out": { "op": "solid", "color": "$bg" } },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    assert_eq!(r.pixel(0, 0), [0x10, 0x20, 0x30, 0xff]);
}

#[test]
fn runtime_override_changes_output() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "params": { "bg": { "type": "color", "default": "#102030" } },
      "nodes": { "out": { "op": "solid", "color": "$bg" } },
      "output": "@out"
    }"##;
    let r = render_with_params(
        json,
        8,
        0,
        Z0,
        &[("bg", ScalarValue::Color([1.0, 0.0, 0.0, 1.0]))],
    );
    assert_eq!(r.pixel(0, 0), [0xff, 0x00, 0x00, 0xff]);
}

/// A full-tile polygon whose `fill-alpha` is a `$param`.
fn alpha_doc() -> &'static str {
    r##"{
      "name": "demo",
      "tile-size": 8,
      "params": { "a": { "type": "number", "default": 1.0, "min": 0, "max": 1 } },
      "nodes": {
        "shape": { "op": "literal-geometry",
                   "polygons": [ { "exterior":
                     [[0, 0], [4096, 0], [4096, 4096], [0, 4096], [0, 0]] } ] },
        "out":   { "op": "fill-solid", "features": "@shape",
                   "fill": "#ffffff", "fill-alpha": "$a" }
      },
      "output": "@out"
    }"##
}

#[test]
fn number_param_override_and_clamp() {
    // Default: opaque white.
    let r = render(alpha_doc(), 8, 0);
    assert_eq!(r.pixel(4, 4)[3], 0xff);

    // Override to 0: fully transparent.
    let r = render_with_params(alpha_doc(), 8, 0, Z0, &[("a", ScalarValue::Number(0.0))]);
    assert_eq!(r.pixel(4, 4)[3], 0x00);

    // Out-of-range override clamps to the declared max (1.0).
    let r = render_with_params(alpha_doc(), 8, 0, Z0, &[("a", ScalarValue::Number(5.0))]);
    assert_eq!(r.pixel(4, 4)[3], 0xff);
}

#[test]
fn math_node_feeds_scalar_port() {
    // fill-alpha = $a * 0.5, wired through a `math` node port.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "params": { "a": { "type": "number", "default": 1.0, "min": 0, "max": 1 } },
      "nodes": {
        "half":  { "op": "math", "fn": "mul", "a": "$a", "b": 0.5 },
        "shape": { "op": "literal-geometry",
                   "polygons": [ { "exterior":
                     [[0, 0], [4096, 0], [4096, 4096], [0, 4096], [0, 0]] } ] },
        "out":   { "op": "fill-solid", "features": "@shape",
                   "fill": "#ffffff", "fill-alpha": "@half" }
      },
      "output": "@out"
    }"##;
    // Default a=1 -> alpha 0.5.
    let r = render(json, 8, 0);
    assert!(
        (r.pixel(4, 4)[3] as i32 - 0x80).abs() <= 1,
        "got {:?}",
        r.pixel(4, 4)
    );

    // Override a=0.5 -> alpha 0.25.
    let r = render_with_params(json, 8, 0, Z0, &[("a", ScalarValue::Number(0.5))]);
    assert!(
        (r.pixel(4, 4)[3] as i32 - 0x40).abs() <= 1,
        "got {:?}",
        r.pixel(4, 4)
    );
}

#[test]
fn zoom_node_makes_output_zoom_dependent() {
    // fill-alpha = zoom / 16: more opaque at deeper zooms.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "nodes": {
        "z":     { "op": "zoom" },
        "a":     { "op": "math", "fn": "div", "a": "@z", "b": 16 },
        "shape": { "op": "literal-geometry",
                   "polygons": [ { "exterior":
                     [[0, 0], [4096, 0], [4096, 4096], [0, 4096], [0, 0]] } ] },
        "out":   { "op": "fill-solid", "features": "@shape",
                   "fill": "#ffffff", "fill-alpha": "@a" }
      },
      "output": "@out"
    }"##;
    let at = |z: u8| {
        crate::common::render_tile(json, 8, 0, TileId { z, x: 0, y: 0 }).pixel(4, 4)[3] as f64
    };
    let a4 = at(4);
    let a8 = at(8);
    assert!(
        (a4 / 255.0 - 0.25).abs() < 0.02 && (a8 / 255.0 - 0.5).abs() < 0.02,
        "z4 -> {a4}, z8 -> {a8}"
    );
}

#[test]
fn blur_sigma_param_requires_max() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "params": { "s": { "type": "number", "default": 2.0 } },
      "nodes": {
        "bg":  { "op": "solid", "color": "#ffffff" },
        "out": { "op": "blur", "input": "@bg", "sigma": "$s" }
      },
      "output": "@out"
    }"##;
    let doc = Document::from_json(json).unwrap();
    let err = build_graph(&doc, &default_registry()).unwrap_err();
    assert!(
        err.to_string().contains("sigma"),
        "expected pad-bound error, got: {err}"
    );

    // Same doc with `max` declared builds fine.
    let ok = json.replace(r#""default": 2.0"#, r#""default": 2.0, "max": 8"#);
    let doc = Document::from_json(&ok).unwrap();
    build_graph(&doc, &default_registry()).expect("builds with max");
}

#[test]
fn param_type_mismatch_is_a_build_error() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "params": { "a": { "type": "number", "default": 0.5 } },
      "nodes": { "out": { "op": "solid", "color": "$a" } },
      "output": "@out"
    }"##;
    let doc = Document::from_json(json).unwrap();
    let err = build_graph(&doc, &default_registry()).unwrap_err();
    assert!(
        err.to_string().contains("color"),
        "expected type mismatch error, got: {err}"
    );
}

#[test]
fn shared_cache_distinguishes_param_values() {
    // One cache, two renders with different param values: the second
    // render must NOT reuse the first one's cached output.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "params": { "bg": { "type": "color", "default": "#102030" } },
      "nodes": { "out": { "op": "solid", "color": "$bg" } },
      "output": "@out"
    }"##;
    let doc = Document::from_json(json).unwrap();
    let registry = default_registry();
    let graph = build_graph(&doc, &registry).unwrap();
    let cache = Cache::new();
    let ev = Evaluator::new(&graph, &cache, &NoAssets);
    let canvas = CanvasInfo::square(8, 0);

    let render_px = |pv: &ParamValues| {
        let out = ev.render(Z0, canvas, pv, 0).unwrap();
        match out {
            PortValue::Raster(r) => r.pixel(0, 0),
            other => panic!("expected raster, got {:?}", other.kind()),
        }
    };

    let defaults = render_px(&ParamValues::new());
    assert_eq!(defaults, [0x10, 0x20, 0x30, 0xff]);

    let mut red = ParamValues::new();
    red.set("bg", ScalarValue::Color([1.0, 0.0, 0.0, 1.0]));
    assert_eq!(render_px(&red), [0xff, 0x00, 0x00, 0xff]);

    // And back: the original entry is still valid in the cache.
    assert_eq!(render_px(&ParamValues::new()), [0x10, 0x20, 0x30, 0xff]);
}

/// A padding-determining field takes an `@node` port when the style
/// declares the ceiling padding is computed from.
///
/// Without one there is nothing to size the canvas by, since a computed
/// value does not exist until the tile renders — and a computed value
/// cannot be a `$param`, so `<field>-max` is the only way to say it.
#[test]
fn padding_field_accepts_a_port_with_a_declared_ceiling() {
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "nodes": {
        "z":   { "op": "zoom" },
        "s":   { "op": "math", "fn": "mul", "a": "@z", "b": 1.0 },
        "bg":  { "op": "solid", "color": "#ffffff" },
        "out": { "op": "blur", "input": "@bg", "sigma": "@s", "sigma-max": 6 }
      },
      "output": "@out"
    }"##;
    let doc = Document::from_json(json).unwrap();
    let graph = build_graph(&doc, &default_registry()).expect("builds with a ceiling");
    // Padding comes from the ceiling, not from whatever the port yields.
    assert_eq!(graph.required_pad().unwrap(), 18, "3 × sigma-max");

    // And it renders: z=4 asks for sigma 4, inside the ceiling.
    let r = render_tile(json, 16, 18, TileId { z: 4, x: 0, y: 0 });
    assert_eq!(r.pixel(8, 8), [0xff, 0xff, 0xff, 0xff]);
}

#[test]
fn padding_field_rejects_a_port_with_no_ceiling() {
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "nodes": {
        "z":   { "op": "zoom" },
        "bg":  { "op": "solid", "color": "#ffffff" },
        "out": { "op": "blur", "input": "@bg", "sigma": "@z" }
      },
      "output": "@out"
    }"##;
    let doc = Document::from_json(json).unwrap();
    let err = build_graph(&doc, &default_registry())
        .expect_err("a port with no bound cannot size the canvas");
    let msg = err.to_string();
    assert!(
        msg.contains("sigma-max"),
        "the error should name the way out, got: {msg}"
    );
}

/// A port that exceeds the declared ceiling is clamped to it: the canvas
/// was padded for the ceiling, and reading past the margin would sample
/// clamped edge pixels instead — a worse lie than a weaker blur.
#[test]
fn padding_field_clamps_a_port_above_its_ceiling() {
    let doc_at = |ceiling: f64| {
        format!(
            r##"{{
      "name": "demo",
      "tile-size": 16,
      "nodes": {{
        "z":   {{ "op": "zoom" }},
        "s":   {{ "op": "math", "fn": "mul", "a": "@z", "b": 4.0 }},
        "dot": {{ "op": "circle", "color": "#000000", "radius-frac": 0.2 }},
        "out": {{ "op": "blur", "input": "@dot", "sigma": "@s", "sigma-max": {ceiling} }}
      }},
      "output": "@out"
    }}"##
        )
    };
    // z=2 asks for sigma 8. Clamped to 2, the render must match a
    // literal sigma of 2 — the ceiling, not the request.
    let clamped = render_tile(&doc_at(2.0), 16, 24, TileId { z: 2, x: 0, y: 0 });
    let literal = render_tile(
        &doc_at(2.0).replace(r#""sigma": "@s""#, r#""sigma": 2"#),
        16,
        24,
        TileId { z: 2, x: 0, y: 0 },
    );
    assert_eq!(
        clamped.pixels, literal.pixels,
        "a port above its ceiling should render as the ceiling"
    );
}
