//! Shared helpers for end-to-end integration tests.

use ezu_graph::{
    build_graph, AssetLoader, Cache, CanvasInfo, Evaluator, NoAssets, ParamValues, PortValue,
    RasterBuf, TileId,
};
use ezu_paint::host::BrushBankLoader;
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
    render_with_assets(json, tile_size, pad, tile, &NoAssets)
}

/// Render with caller-supplied runtime parameter values.
#[allow(dead_code)]
pub fn render_with_params(
    json: &str,
    tile_size: u32,
    pad: u32,
    tile: TileId,
    params: &[(&str, ezu_graph::ScalarValue)],
) -> std::sync::Arc<ezu_graph::RasterBuf> {
    let mut pv = ParamValues::new();
    for (name, value) in params {
        pv.set(name.to_string(), *value);
    }
    let doc = Document::from_json(json).expect("parse");
    let registry = default_registry();
    let graph = build_graph(&doc, &registry).expect("build");
    let cache = Cache::new();
    let ev = Evaluator::new(&graph, &cache, &NoAssets);
    let out = ev
        .render(tile, CanvasInfo { tile_size, pad }, &pv, 0)
        .expect("render");
    match out {
        PortValue::Raster(r) => r,
        other => panic!("expected raster output, got {:?}", other.kind()),
    }
}

/// Render with an in-memory image bank. `images` maps asset name to
/// the `RasterBuf` returned by the loader for that name. Test-only
/// helper for `image` / `stamp` / `tiling` / `place` paths.
#[allow(dead_code)]
pub fn render_with_images(
    json: &str,
    tile_size: u32,
    pad: u32,
    tile: TileId,
    images: &[(&str, RasterBuf)],
) -> std::sync::Arc<ezu_graph::RasterBuf> {
    let mut loader = BrushBankLoader::new();
    for (name, img) in images {
        loader.insert_image(name.to_string(), img.clone());
    }
    render_with_assets(json, tile_size, pad, tile, &loader)
}

/// Render with tile-scoped raster bindings (bare source names), the
/// way a host binds stitched imagery via `TileLoader::bind_raster`.
#[allow(dead_code)]
pub fn render_with_rasters(
    json: &str,
    tile_size: u32,
    pad: u32,
    tile: TileId,
    rasters: &[(&str, RasterBuf)],
) -> std::sync::Arc<ezu_graph::RasterBuf> {
    use ezu_paint::host::TileLoader;
    let doc = Document::from_json(json).expect("parse");
    let registry = default_registry();
    let graph = build_graph(&doc, &registry).expect("build");
    let cache = Cache::new();
    let base = NoAssets;
    let mut loader = TileLoader::new(&base, tile);
    for (name, buf) in rasters {
        loader.bind_raster(name.to_string(), buf.clone());
    }
    let ev = Evaluator::new(&graph, &cache, &loader);
    let out = ev
        .render(tile, CanvasInfo { tile_size, pad }, &ParamValues::new(), 0)
        .expect("render");
    match out {
        PortValue::Raster(r) => r,
        other => panic!("expected raster output, got {:?}", other.kind()),
    }
}

fn render_with_assets(
    json: &str,
    tile_size: u32,
    pad: u32,
    tile: TileId,
    assets: &dyn AssetLoader,
) -> std::sync::Arc<ezu_graph::RasterBuf> {
    let doc = Document::from_json(json).expect("parse");
    let registry = default_registry();
    let graph = build_graph(&doc, &registry).expect("build");
    let cache = Cache::new();
    let ev = Evaluator::new(&graph, &cache, assets);
    let out = ev
        .render(tile, CanvasInfo { tile_size, pad }, &ParamValues::new(), 0)
        .expect("render");
    match out {
        PortValue::Raster(r) => r,
        other => panic!("expected raster output, got {:?}", other.kind()),
    }
}

/// Build a tiny test sprite filled with the given premultiplied RGBA8
/// color. `(w, h)` are pixel dimensions. Used by raster_layout tests
/// to feed `image` without touching the disk.
#[allow(dead_code)]
pub fn solid_sprite(w: u32, h: u32, rgba_premul: [u8; 4]) -> RasterBuf {
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        pixels.extend_from_slice(&rgba_premul);
    }
    RasterBuf {
        width: w,
        height: h,
        pixels,
    }
}

/// Build a test sprite of size `w × h` with a centered solid-color
/// disk of `radius` pixels (in src-image coords). Outside the disk is
/// transparent. Color is premultiplied RGBA8.
#[allow(dead_code)]
pub fn disk_sprite(w: u32, h: u32, radius: f32, rgba_premul: [u8; 4]) -> RasterBuf {
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            if (dx * dx + dy * dy).sqrt() <= radius {
                let i = ((y * w + x) * 4) as usize;
                pixels[i..i + 4].copy_from_slice(&rgba_premul);
            }
        }
    }
    RasterBuf {
        width: w,
        height: h,
        pixels,
    }
}
