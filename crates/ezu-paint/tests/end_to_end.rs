//! End-to-end: JSON -> typed graph -> evaluated tile -> pixels.

#[test]
fn registry_emits_document_schema_with_all_ops() {
    let registry = ezu_paint::nodes::default_registry();
    let schema = registry.document_schema();
    let s = schema.to_string();
    // Spot-check: every built-in op surfaces in the schema and the
    // document-level structure is there.
    for op in [
        "solid",
        "circle",
        "blur",
        "blend",
        "brightness-contrast",
        "hsl",
        "invert",
        "color-to-alpha",
        "mvt-source",
        "fill-solid",
        "fill-dabs",
        "line",
        "brush-file",
    ] {
        assert!(s.contains(&format!("\"const\":\"{op}\"")), "missing op `{op}` in schema");
    }
    assert!(s.contains("\"$schema\""));
    assert!(s.contains("\"nodes\""));
    assert!(s.contains("\"output\""));
}

use ezu_graph::{
    build_graph, Cache, CanvasInfo, Evaluator, NoAssets, ParamValues, PortValue, TileId,
};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

fn render(json: &str, tile_size: u32, pad: u32) -> std::sync::Arc<ezu_graph::RasterBuf> {
    let doc = Document::from_json(json).expect("parse");
    let registry = default_registry();
    let graph = build_graph(&doc, &registry).expect("build");
    let cache = Cache::new();
    let assets = NoAssets;
    let ev = Evaluator::new(&graph, &cache, &assets);
    let out = ev
        .render(
            TileId { z: 0, x: 0, y: 0 },
            CanvasInfo { tile_size, pad },
            &ParamValues::new(),
            0,
        )
        .expect("render");
    match out {
        PortValue::Raster(r) => r,
        other => panic!("expected raster output, got {:?}", other.kind()),
    }
}

#[test]
fn solid_only_produces_uniform_raster() {
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "nodes": { "bg": { "op": "solid", "color": "#3366ff" } },
      "output": "@bg"
    }"##;
    let r = render(json, 16, 0);
    assert_eq!(r.width, 16);
    let p = r.pixel(8, 8);
    assert_eq!(p, [0x33, 0x66, 0xff, 0xff]);
}

#[test]
fn circle_fill_then_blend_over_background() {
    // Background is opaque red. A blue disk drawn at center, blended on top.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "bg":    { "op": "solid", "color": "#ff0000" },
        "blue":  { "op": "circle", "color": "#0000ff", "radius-frac": 0.4 },
        "out":   { "op": "blend", "base": "@bg", "over": "@blue" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Center pixel should be blue (mask = 1).
    let center = r.pixel(16, 16);
    assert!(center[2] > 200, "center should be blue-dominant: {center:?}");
    assert!(center[0] < 32, "center red should be near zero: {center:?}");
    // Corner pixel should be red (outside disk).
    let corner = r.pixel(0, 0);
    assert_eq!(corner, [0xff, 0x00, 0x00, 0xff]);
}

#[test]
fn blur_softens_disk_edge() {
    let json_sharp = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "fill":  { "op": "circle", "color": "#000000ff", "radius-frac": 0.4 }
      },
      "output": "@fill"
    }"##;
    let json_blur = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "disk":  { "op": "circle", "color": "#000000ff", "radius-frac": 0.4 },
        "fill":  { "op": "blur", "input": "@disk", "sigma": 1.5 }
      },
      "output": "@fill"
    }"##;
    let sharp = render(json_sharp, 32, 0);
    let blur = render(json_blur, 32, 0);
    // A pixel just outside the disk edge should be more covered by the
    // blurred version (alpha > 0) but transparent in the sharp one.
    // radius = 32 * 0.4 = 12.8 → check pixel at (16+13, 16) ≈ outside.
    let px_sharp = sharp.pixel(29, 16);
    let px_blur = blur.pixel(29, 16);
    assert_eq!(px_sharp[3], 0, "outside the disk, sharp version is transparent");
    assert!(
        px_blur[3] > 0,
        "outside the disk, blurred version has some coverage: {px_blur:?}"
    );
}

#[test]
fn blend_multiply_darkens_base() {
    // Multiply two opaque mid-grays: 0x80 * 0x80 / 0xff ≈ 0x40.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "nodes": {
        "a":   { "op": "solid", "color": "#808080" },
        "b":   { "op": "solid", "color": "#808080" },
        "out": { "op": "blend", "base": "@a", "over": "@b", "mode": "multiply" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    // Expect close to 0x40 (64). Allow ±2 for rounding.
    assert!((p[0] as i32 - 0x40).abs() <= 2, "got {p:?}");
    assert_eq!(p[3], 0xff, "fully opaque");
}

#[test]
fn blend_clip_confines_to_base_alpha() {
    // base is a circle (alpha varies); over is solid red. With clip,
    // pixels outside the circle stay transparent.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "base": { "op": "circle", "color": "#0000ff", "radius-frac": 0.3 },
        "over": { "op": "solid", "color": "#ff0000" },
        "out":  { "op": "blend", "base": "@base", "over": "@over", "clip": true }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Corner is outside the disk → base alpha = 0 → clip output alpha = 0.
    let corner = r.pixel(0, 0);
    assert_eq!(corner[3], 0, "outside base alpha must be 0 under clip: {corner:?}");
    // Center is inside disk → red shows through atop blue's alpha.
    let center = r.pixel(16, 16);
    assert!(center[3] > 200, "center should be opaque: {center:?}");
    assert!(center[0] > 200, "center should be red-dominant: {center:?}");
}

#[test]
fn blend_mask_modulates_over_coverage() {
    // mask is a small disk; using it as the mask input means red over
    // only appears where the mask is opaque.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "base": { "op": "solid", "color": "#0000ff" },
        "over": { "op": "solid", "color": "#ff0000" },
        "mask": { "op": "circle", "color": "#ffffff", "radius-frac": 0.3 },
        "out":  { "op": "blend", "base": "@base", "over": "@over", "mask": "@mask" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Outside mask → still pure blue.
    let corner = r.pixel(0, 0);
    assert_eq!(corner, [0x00, 0x00, 0xff, 0xff]);
    // Inside mask → red wins.
    let center = r.pixel(16, 16);
    assert!(center[0] > 200, "center should be red: {center:?}");
    assert!(center[2] < 32, "center blue should be near zero: {center:?}");
}

#[test]
fn invert_negates_rgb_preserving_alpha() {
    let json = r##"{
      "name": "demo",
      "tile-size": 4,
      "nodes": {
        "src": { "op": "solid", "color": "#204060" },
        "out": { "op": "invert", "input": "@src" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 4, 0);
    // 0x20 -> 0xdf, 0x40 -> 0xbf, 0x60 -> 0x9f.
    assert_eq!(r.pixel(0, 0), [0xdf, 0xbf, 0x9f, 0xff]);
}

#[test]
fn brightness_contrast_shifts_levels() {
    let json = r##"{
      "name": "demo",
      "tile-size": 4,
      "nodes": {
        "src": { "op": "solid", "color": "#808080" },
        "out": { "op": "brightness-contrast", "input": "@src", "brightness": 0.25 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 4, 0);
    // 0.5 + 0.25 = 0.75 -> ~0xbf.
    let p = r.pixel(0, 0);
    assert!((p[0] as i32 - 0xbf).abs() <= 2, "got {p:?}");
}

#[test]
fn hsl_hue_shift_rotates_color() {
    // Pure red rotated by +120° -> pure green.
    let json = r##"{
      "name": "demo",
      "tile-size": 4,
      "nodes": {
        "src": { "op": "solid", "color": "#ff0000" },
        "out": { "op": "hsl", "input": "@src", "hue-shift": 120 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 4, 0);
    let p = r.pixel(0, 0);
    assert!(p[1] > 240 && p[0] < 8 && p[2] < 8, "expected pure green: {p:?}");
}

#[test]
fn color_to_alpha_keys_out_target() {
    // Red surface: keying red drops alpha to 0, keying blue leaves it.
    let json_keyed = r##"{
      "name": "demo",
      "tile-size": 4,
      "nodes": {
        "src": { "op": "solid", "color": "#ff0000" },
        "out": { "op": "color-to-alpha", "input": "@src", "color": "#ff0000", "threshold": 0.05, "softness": 0.05 }
      },
      "output": "@out"
    }"##;
    let json_kept = r##"{
      "name": "demo",
      "tile-size": 4,
      "nodes": {
        "src": { "op": "solid", "color": "#ff0000" },
        "out": { "op": "color-to-alpha", "input": "@src", "color": "#0000ff", "threshold": 0.05, "softness": 0.05 }
      },
      "output": "@out"
    }"##;
    let keyed = render(json_keyed, 4, 0);
    let kept = render(json_kept, 4, 0);
    assert_eq!(keyed.pixel(0, 0)[3], 0, "matching color should be transparent");
    assert_eq!(kept.pixel(0, 0)[3], 0xff, "distant color should be opaque");
}

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
