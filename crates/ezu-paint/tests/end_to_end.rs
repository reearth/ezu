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
