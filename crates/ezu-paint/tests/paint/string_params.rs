//! `string` params driving fields that take one of a fixed set of names:
//! read per render where the field allows it, baked in with a warning
//! where the graph needs the value before it can evaluate, and checked
//! against the field's names when the graph is built.

use crate::common::{render, render_with_params};

use ezu_graph::{
    build_graph, Cache, CanvasInfo, Evaluator, NoAssets, ParamValues, PortValue, ScalarValue,
    TileId,
};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

const Z0: TileId = TileId { z: 0, x: 0, y: 0 };

/// A red square blended over a grey one by `mode`, which is spliced in
/// verbatim, with `params` declared alongside.
fn blend_doc(params: &str, mode: &str) -> String {
    format!(
        r##"{{
          "name": "blend-mode",
          "tile-size": 8,
          "params": {{ {params} }},
          "nodes": {{
            "base": {{ "op": "solid", "color": "#808080" }},
            "over": {{ "op": "solid", "color": "#ff4020" }},
            "out":  {{ "op": "blend", "base": "@base", "over": "@over", "mode": {mode} }}
          }},
          "output": "@out"
        }}"##
    )
}

const MODE_PARAM: &str = r#""mode": { "type": "string", "enum": ["normal", "multiply", "screen"], "default": "multiply" }"#;

fn string(s: &str) -> ScalarValue {
    ScalarValue::String(s.into())
}

fn build_error(json: &str) -> String {
    let doc = Document::from_json(json).expect("parse");
    match build_graph(&doc, &default_registry()) {
        Ok(_) => panic!("built"),
        Err(e) => e.to_string(),
    }
}

fn warnings(json: &str) -> Vec<String> {
    let doc = Document::from_json(json).expect("parse");
    build_graph(&doc, &default_registry())
        .expect("build")
        .warnings()
        .to_vec()
}

#[test]
fn a_string_param_sets_the_blend_mode_per_render() {
    let literal = |m: &str| render(&blend_doc("", &format!("\"{m}\"")), 8, 0);
    let doc = blend_doc(MODE_PARAM, "\"$mode\"");
    // Unset, the declared default applies.
    assert_eq!(render(&doc, 8, 0).pixels, literal("multiply").pixels);
    for m in ["normal", "screen"] {
        let r = render_with_params(&doc, 8, 0, Z0, &[("mode", string(m))]);
        assert_eq!(r.pixels, literal(m).pixels, "{m}");
    }
    // Reading the field through a param is not a build-time read.
    assert!(warnings(&doc).is_empty(), "{:?}", warnings(&doc));
}

/// One graph and one cache, rendered under two values: the second must
/// not be served the first's cached tile.
#[test]
fn the_cache_keys_on_a_string_params_value() {
    let doc = Document::from_json(&blend_doc(MODE_PARAM, "\"$mode\"")).unwrap();
    let graph = build_graph(&doc, &default_registry()).unwrap();
    let cache = Cache::new();
    let ev = Evaluator::new(&graph, &cache, &NoAssets);
    let draw = |m: &str| {
        let mut pv = ParamValues::new();
        pv.set("mode", string(m));
        match ev.render(Z0, CanvasInfo::square(8, 0), &pv, 0).unwrap() {
            PortValue::Raster(r) => r.pixels.clone(),
            other => panic!("{:?}", other.kind()),
        }
    };
    let multiply = draw("multiply");
    let screen = draw("screen");
    assert_ne!(multiply, screen);
    assert_eq!(draw("multiply"), multiply);
}

#[test]
fn a_param_must_be_a_string_that_the_field_accepts() {
    // Its enum allows a name `blend` has no mode for.
    let msg = build_error(&blend_doc(
        r#""mode": { "type": "string", "enum": ["multiply", "plus-lighter"], "default": "multiply" }"#,
        "\"$mode\"",
    ));
    assert!(msg.contains("allows `plus-lighter`"), "{msg}");
    // Its default is one.
    let msg = build_error(&blend_doc(
        r#""mode": { "type": "string", "default": "plus-lighter" }"#,
        "\"$mode\"",
    ));
    assert!(msg.contains("defaults to `plus-lighter`"), "{msg}");
    // It is not a string at all.
    let msg = build_error(&blend_doc(
        r#""mode": { "type": "number", "default": 1 }"#,
        "\"$mode\"",
    ));
    assert!(msg.contains("declared `number`"), "{msg}");
    // A default outside the param's own enum fails before any field reads it.
    let msg = build_error(&blend_doc(
        r#""mode": { "type": "string", "enum": ["screen"], "default": "multiply" }"#,
        "\"normal\"",
    ));
    assert!(msg.contains("not one of its `enum`"), "{msg}");
}

/// Without an `enum` any string reaches the node, so a name the field
/// does not know fails the render that sets it.
#[test]
fn an_unlisted_value_fails_the_render() {
    let doc = Document::from_json(&blend_doc(
        r#""mode": { "type": "string", "default": "multiply" }"#,
        "\"$mode\"",
    ))
    .unwrap();
    let graph = build_graph(&doc, &default_registry()).unwrap();
    let cache = Cache::new();
    let mut pv = ParamValues::new();
    pv.set("mode", string("plus-lighter"));
    let err = Evaluator::new(&graph, &cache, &NoAssets)
        .render(Z0, CanvasInfo::square(8, 0), &pv, 0)
        .expect_err("an unknown mode");
    assert!(err.to_string().contains("`plus-lighter`"), "{err}");
}

/// A stroke's cap follows the param, so a dot drawn with `round` caps
/// differs from one with `butt` caps.
#[test]
fn a_string_param_sets_the_stroke_cap() {
    let doc = |cap: &str| {
        format!(
            r##"{{
              "name": "cap",
              "tile-size": 32,
              "params": {{ "cap": {{ "type": "string", "enum": ["butt", "round", "square"], "default": "butt" }} }},
              "nodes": {{
                "line": {{ "op": "literal-geometry", "lines": [[[1024, 2048], [3072, 2048]]] }},
                "out":  {{ "op": "stroke", "features": "@line", "color": "#000000",
                          "width-px": 8, "cap": {cap} }}
              }},
              "output": "@out"
            }}"##
        )
    };
    let by_param = doc("\"$cap\"");
    for cap in ["butt", "round", "square"] {
        let r = render_with_params(&by_param, 32, 0, Z0, &[("cap", string(cap))]);
        assert_eq!(
            r.pixels,
            render(&doc(&format!("\"{cap}\"")), 32, 0).pixels,
            "{cap}"
        );
    }
    assert_ne!(
        render(&doc("\"butt\""), 32, 0).pixels,
        render(&doc("\"square\""), 32, 0).pixels
    );
}

/// `math`'s `fn` decides how many operands the node has, so it is fixed
/// when the graph is built: a param gives its default, and the build says
/// that an override will not reach it.
#[test]
fn a_build_time_field_bakes_the_default_in() {
    let json = r##"{
      "name": "baked",
      "tile-size": 8,
      "params": { "f": { "type": "string", "enum": ["add", "mul"], "default": "mul" } },
      "nodes": {
        "k":    { "op": "math", "fn": "$f", "a": 0.25, "b": 0.5 },
        "base": { "op": "solid", "color": "#808080" },
        "over": { "op": "solid", "color": "#ff4020" },
        "out":  { "op": "blend", "base": "@base", "over": "@over", "opacity": "@k" }
      },
      "output": "@out"
    }"##;
    let w = warnings(json);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].contains("`$f` is read when the graph is built"),
        "{}",
        w[0]
    );
    // The default `mul`, whatever the override says.
    let overridden = render_with_params(json, 8, 0, Z0, &[("f", string("add"))]);
    let mul = render(&json.replace("\"$f\"", "\"mul\""), 8, 0);
    let add = render(&json.replace("\"$f\"", "\"add\""), 8, 0);
    assert_eq!(render(json, 8, 0).pixels, mul.pixels);
    assert_eq!(overridden.pixels, mul.pixels);
    assert_ne!(add.pixels, mul.pixels);
    // The same checks apply to a baked field as to a per-render one.
    let msg = build_error(&json.replace(r#"["add", "mul"]"#, r#"["add", "mul", "hypot"]"#));
    assert!(msg.contains("allows `hypot`"), "{msg}");
}
