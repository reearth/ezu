//! Shared helpers for end-to-end integration tests.

use ezu_graph::{
    build_graph, Cache, CanvasInfo, Evaluator, NoAssets, ParamValues, PortValue, TileId,
};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

#[allow(dead_code)]
pub fn render(json: &str, tile_size: u32, pad: u32) -> std::sync::Arc<ezu_graph::RasterBuf> {
    render_tile(json, tile_size, pad, TileId { z: 0, x: 0, y: 0 })
}

#[allow(dead_code)]
pub fn render_tile(
    json: &str,
    tile_size: u32,
    pad: u32,
    tile: TileId,
) -> std::sync::Arc<ezu_graph::RasterBuf> {
    let doc = Document::from_json(json).expect("parse");
    let registry = default_registry();
    let graph = build_graph(&doc, &registry).expect("build");
    let cache = Cache::new();
    let assets = NoAssets;
    let ev = Evaluator::new(&graph, &cache, &assets);
    let out = ev
        .render(tile, CanvasInfo { tile_size, pad }, &ParamValues::new(), 0)
        .expect("render");
    match out {
        PortValue::Raster(r) => r,
        other => panic!("expected raster output, got {:?}", other.kind()),
    }
}
