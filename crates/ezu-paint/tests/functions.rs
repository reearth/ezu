//! User-defined functions: end-to-end expansion + render parity,
//! `$param` arguments, declared-kind verification, recursion errors.

mod common;
use common::{render, render_with_params};

use ezu_graph::{build_graph, ScalarValue, TileId};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

const Z0: TileId = TileId { z: 0, x: 0, y: 0 };

/// A function style and its hand-inlined equivalent must render
/// pixel-identically.
#[test]
fn function_render_matches_hand_inlined() {
    let with_fn = r##"{
      "name": "demo",
      "tile-size": 8,
      "functions": {
        "tinted": {
          "inputs": {
            "base":  { "kind": "raster" },
            "color": { "kind": "scalar" },
            "alpha": { "kind": "scalar", "default": 0.5 }
          },
          "output": "@mix",
          "output-kind": "raster",
          "nodes": {
            "tint": { "op": "solid", "color": "@color" },
            "mix":  { "op": "blend", "base": "@base", "over": "@tint",
                      "opacity": "@alpha" }
          }
        }
      },
      "nodes": {
        "bg":  { "op": "solid", "color": "#204060" },
        "out": { "op": "func", "fn": "tinted", "base": "@bg", "color": "#ff0000" }
      },
      "output": "@out"
    }"##;
    let inlined = r##"{
      "name": "demo",
      "tile-size": 8,
      "nodes": {
        "bg":   { "op": "solid", "color": "#204060" },
        "tint": { "op": "solid", "color": "#ff0000" },
        "out":  { "op": "blend", "base": "@bg", "over": "@tint", "opacity": 0.5 }
      },
      "output": "@out"
    }"##;
    let a = render(with_fn, 8, 0);
    let b = render(inlined, 8, 0);
    assert_eq!(a.pixel(4, 4), b.pixel(4, 4));
    assert_eq!(a.pixels, b.pixels);
}

/// A `$param` argument keeps its runtime-override behavior inside the
/// expanded body.
#[test]
fn param_arg_flows_through_function() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "params": { "ink": { "type": "color", "default": "#102030" } },
      "functions": {
        "fill": {
          "inputs": { "color": { "kind": "scalar" } },
          "output": "@n",
          "output-kind": "raster",
          "nodes": { "n": { "op": "solid", "color": "@color" } }
        }
      },
      "nodes": { "out": { "op": "func", "fn": "fill", "color": "$ink" } },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    assert_eq!(r.pixel(0, 0), [0x10, 0x20, 0x30, 0xff]);
    let r = render_with_params(
        json,
        8,
        0,
        Z0,
        &[("ink", ScalarValue::Color([1.0, 0.0, 0.0, 1.0]))],
    );
    assert_eq!(r.pixel(0, 0), [0xff, 0x00, 0x00, 0xff]);
}

/// Nested calls expand and render; the inner function feeds the outer.
#[test]
fn nested_function_renders() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "functions": {
        "red": {
          "inputs": {},
          "output": "@n",
          "output-kind": "raster",
          "nodes": { "n": { "op": "solid", "color": "#ff0000" } }
        },
        "dimmed-red": {
          "inputs": { "base": { "kind": "raster" } },
          "output": "@mix",
          "output-kind": "raster",
          "nodes": {
            "r":   { "op": "func", "fn": "red" },
            "mix": { "op": "blend", "base": "@base", "over": "@r", "opacity": 0.5 }
          }
        }
      },
      "nodes": {
        "bg":  { "op": "solid", "color": "#000000" },
        "out": { "op": "func", "fn": "dimmed-red", "base": "@bg" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    assert!(
        p[0] > 0x60 && p[0] < 0xa0 && p[1] == 0 && p[2] == 0,
        "got {p:?}"
    );
}

#[test]
fn recursive_call_is_a_build_error() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "functions": {
        "a": { "inputs": {}, "output": "@n", "output-kind": "raster",
               "nodes": { "n": { "op": "func", "fn": "b" } } },
        "b": { "inputs": {}, "output": "@n", "output-kind": "raster",
               "nodes": { "n": { "op": "func", "fn": "a" } } }
      },
      "nodes": { "out": { "op": "func", "fn": "a" } },
      "output": "@out"
    }"##;
    let doc = Document::from_json(json).unwrap();
    let err = build_graph(&doc, &default_registry()).unwrap_err();
    assert!(
        err.to_string().contains("recursive"),
        "expected recursion error, got: {err}"
    );
}

#[test]
fn declared_input_kind_is_verified_at_the_call_site() {
    // `base` declares `features`, but the argument is a raster node.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "functions": {
        "f": {
          "inputs": { "base": { "kind": "features" } },
          "output": "@n",
          "output-kind": "raster",
          "nodes": { "n": { "op": "fill-solid", "features": "@base", "fill": "#ffffff" } }
        }
      },
      "nodes": {
        "bg":  { "op": "solid", "color": "#ffffff" },
        "out": { "op": "func", "fn": "f", "base": "@bg" }
      },
      "output": "@out"
    }"##;
    let doc = Document::from_json(json).unwrap();
    let err = build_graph(&doc, &default_registry()).unwrap_err();
    // The builder's port type check fires first and already names the
    // call site (body nodes carry the call id); the declared-kind check
    // is a backstop for wirings the port check can't see.
    let msg = err.to_string();
    assert!(
        msg.contains("out") && msg.contains("features") && msg.contains("raster"),
        "expected call-site kind error, got: {msg}"
    );
}

#[test]
fn declared_output_kind_is_verified() {
    // Body produces Features, but output-kind claims raster.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "functions": {
        "f": {
          "inputs": {},
          "output": "@n",
          "output-kind": "raster",
          "nodes": { "n": { "op": "literal-geometry", "points": [[0, 0]] } }
        }
      },
      "nodes": {
        "fn_out": { "op": "func", "fn": "f" },
        "out":    { "op": "fill-solid", "features": "@fn_out", "fill": "#ffffff" }
      },
      "output": "@out"
    }"##;
    let doc = Document::from_json(json).unwrap();
    let err = build_graph(&doc, &default_registry()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("output-kind") && msg.contains("features"),
        "expected output-kind error, got: {msg}"
    );
}
