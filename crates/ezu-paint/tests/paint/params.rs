//! Document-level parameters: `$param` substitution, runtime
//! overrides, min/max clamping, `@node` scalar ports (`math`, `zoom`),
//! and cache invalidation across param changes.

use crate::common::{render, render_with_params};

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
    let canvas = CanvasInfo {
        tile_size: 8,
        pad: 0,
    };

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
