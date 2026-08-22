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

// --- `$param` inside a composite field ------------------------------------
//
// A colour ramp's `stops` is a table, not a scalar field, so each half of
// each entry is read through `InReader::nested`: literal or `$param`, and
// resolved once per eval rather than baked at build time.

/// Two-stop ramp over an all-zero `dem` field: every pixel clamps to the
/// first stop, so the rendered colour *is* `$low`.
fn ramp_doc() -> &'static str {
    r##"{
      "name": "demo",
      "tile-size": 8,
      "params": {
        "low":  { "type": "color",  "default": "#ff0000" },
        "high": { "type": "color",  "default": "#0000ff" },
        "top":  { "type": "number", "default": 100, "min": 1, "max": 1000 }
      },
      "sources": {
        "terrain": { "type": "dem",
                     "url": "http://example.invalid/{z}/{x}/{y}.webp",
                     "encoding": "terrarium" }
      },
      "nodes": {
        "dem": { "op": "dem", "name": "tile.terrain" },
        "out": { "op": "color-ramp", "field": "@dem",
                 "stops": [ { "value": 0,      "color": "$low" },
                            { "value": "$top", "color": "$high" } ] }
      },
      "output": "@out"
    }"##
}

#[test]
fn param_in_a_ramp_stop_uses_its_declared_default() {
    let r = render(ramp_doc(), 8, 0);
    assert_eq!(r.pixel(4, 4), [0xff, 0x00, 0x00, 0xff]);
}

#[test]
fn runtime_override_recolours_a_ramp_stop() {
    let r = render_with_params(
        ramp_doc(),
        8,
        0,
        Z0,
        &[("low", ScalarValue::Color([0.0, 1.0, 0.0, 1.0]))],
    );
    assert_eq!(
        r.pixel(4, 4),
        [0x00, 0xff, 0x00, 0xff],
        "overriding `low` should repaint without rebuilding the graph"
    );
}

/// The param in a stop's `value` has to reach `param_refs`, or the
/// evaluator keys the cache without it and serves a stale tile.
#[test]
fn a_stop_value_param_is_folded_into_the_cache_key() {
    // Stops at 0 (`$low`) and `$top` (`$high`) over a zero field. Moving
    // `top` cannot change a clamped-to-first-stop pixel, so drive the
    // field between the stops instead: a `solid` ramped by luminance.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "params": { "top": { "type": "number", "default": 1.0, "min": 0.5, "max": 8.0 } },
      "nodes": {
        "grey": { "op": "solid", "color": "#808080" },
        "out":  { "op": "color-ramp", "field": "@grey",
                  "stops": [ { "value": 0,      "color": "#000000" },
                             { "value": "$top", "color": "#ffffff" } ] }
      },
      "output": "@out"
    }"##;
    let doc = Document::from_json(json).unwrap();
    let graph = build_graph(&doc, &default_registry()).unwrap();
    let cache = Cache::new();
    let ev = Evaluator::new(&graph, &cache, &NoAssets);
    let canvas = CanvasInfo::square(8, 0);

    let render_at = |top: f64| {
        let mut params = ParamValues::new();
        params.set("top", ScalarValue::Number(top));
        match ev.render(Z0, canvas, &params, 0).unwrap() {
            PortValue::Raster(r) => r.pixel(4, 4),
            other => panic!("expected a raster, got {:?}", other.kind()),
        }
    };
    // Luminance 0.5 against a 0→1 ramp is mid-grey; against a 0→8 ramp it
    // sits near the dark end. Same cache, so an unkeyed param shows up as
    // the first answer repeated.
    let near = render_at(1.0);
    let far = render_at(8.0);
    assert_ne!(
        near, far,
        "a `$param` in a stop value must be part of the cache key"
    );
    assert!(
        far[0] < near[0],
        "stretching the ramp should darken mid-grey: {near:?} -> {far:?}"
    );
}

/// The gradient ops share one stop reader, so a param works in the
/// `[t, color]` pair form too — including the position half.
#[test]
fn params_drive_a_gradient_stop_position_and_colour() {
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "params": {
        "edge": { "type": "color",  "default": "#000000" },
        "mid":  { "type": "number", "default": 0.5, "min": 0, "max": 1 }
      },
      "nodes": {
        "out": { "op": "gradient-linear", "start": [0, 0], "end": [1, 0],
                 "stops": [ [0, "$edge"], ["$mid", "#ff0000"], [1, "#ffffff"] ] }
      },
      "output": "@out"
    }"##;
    // Default: stop 0 is black, so the left edge has no green in it.
    // (Pixels sample at their centre, so x=0 is already a little way
    // towards the red stop — hence "nearly", not "exactly", the stop.)
    let base = render(json, 16, 0);
    assert_eq!(base.pixel(0, 8)[1], 0, "got {:?}", base.pixel(0, 8));

    // Recolour the first stop: now the left edge is nearly all green.
    let recoloured = render_with_params(
        json,
        16,
        0,
        Z0,
        &[("edge", ScalarValue::Color([0.0, 1.0, 0.0, 1.0]))],
    );
    assert!(
        recoloured.pixel(0, 8)[1] > 0xe0,
        "got {:?}",
        recoloured.pixel(0, 8)
    );

    // Move the red stop left: x=4/16 = 0.25 is past a mid of 0.2, so it
    // has started fading towards white rather than still climbing to red.
    let moved = render_with_params(json, 16, 0, Z0, &[("mid", ScalarValue::Number(0.2))]);
    assert!(
        moved.pixel(4, 8)[2] > base.pixel(4, 8)[2],
        "moving the red stop left should whiten x=0.25: {:?} -> {:?}",
        base.pixel(4, 8),
        moved.pixel(4, 8)
    );
}

/// Stops out of order used to be a build error. A `$param` position can
/// reorder them at render time, so ordering is a runtime concern now and
/// a declared-out-of-order table renders the same as a sorted one.
#[test]
fn gradient_stops_need_not_be_declared_in_order() {
    let doc = |stops: &str| {
        format!(
            r##"{{
      "name": "demo",
      "tile-size": 16,
      "nodes": {{
        "out": {{ "op": "gradient-linear", "start": [0, 0], "end": [1, 0],
                 "stops": {stops} }}
      }},
      "output": "@out"
    }}"##
        )
    };
    let sorted = render(&doc(r##"[ [0, "#000000"], [1, "#ffffff"] ]"##), 16, 0);
    let jumbled = render(&doc(r##"[ [1, "#ffffff"], [0, "#000000"] ]"##), 16, 0);
    assert_eq!(sorted.pixels, jumbled.pixels);
}

/// `quantize` / `dither` share one palette reader, and the palette is
/// projected into the distance metric's space — so a param has to
/// re-project too, not just re-colour the output.
#[test]
fn param_recolours_a_quantize_palette() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "params": { "ink": { "type": "color", "default": "#0000ff" } },
      "nodes": {
        "grey": { "op": "solid", "color": "#808080" },
        "out":  { "op": "quantize", "input": "@grey", "palette": ["$ink"] }
      },
      "output": "@out"
    }"##;
    // A one-entry palette snaps everything to that entry.
    assert_eq!(render(json, 8, 0).pixel(4, 4), [0x00, 0x00, 0xff, 0xff]);

    let r = render_with_params(
        json,
        8,
        0,
        Z0,
        &[("ink", ScalarValue::Color([1.0, 0.0, 0.0, 1.0]))],
    );
    assert_eq!(r.pixel(4, 4), [0xff, 0x00, 0x00, 0xff]);
}
