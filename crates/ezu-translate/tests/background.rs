//! A `background` layer becomes a `solid`: a zoom curve on
//! `background-color` through an `expr` node, and `background-opacity` and
//! a translucent colour folded into the colour's alpha.

use ezu_graph::{build_graph, Cache, CanvasInfo, Evaluator, NoAssets, ParamValues, TileId};
use ezu_paint::host::TileLoader;
use ezu_translate::maplibre::{convert, ConvertOptions};
use serde_json::{json, Value};

/// Convert a style whose only layer is a background with `paint`.
fn convert_background(paint: Value) -> (Value, Vec<String>) {
    let style = json!({
        "version": 8,
        "name": "bg",
        "sources": { "s": { "type": "vector", "url": "https://example.com/tiles.json" } },
        "layers": [ { "id": "bg", "type": "background", "paint": paint } ]
    });
    let (recipe, report) = convert(&style, &ConvertOptions::default()).unwrap();
    (recipe, report.warnings)
}

/// The recipe's top-left pixel at zoom `z`.
fn pixel_at(recipe: &Value, z: u8) -> [u8; 4] {
    let doc = ezu_style::Document::from_json(&serde_json::to_string(recipe).unwrap()).unwrap();
    let graph = build_graph(&doc, &ezu_paint::nodes::default_registry()).unwrap();
    let cache = Cache::new();
    let tile = TileId { z, x: 0, y: 0 };
    let loader = TileLoader::new(&NoAssets, tile);
    let out = Evaluator::new(&graph, &cache, &loader)
        .render(tile, CanvasInfo::square(16, 0), &ParamValues::new(), 0)
        .unwrap();
    let r = out.as_raster().expect("a raster");
    r.pixels[..4].try_into().unwrap()
}

#[test]
fn a_zoom_curve_colour_follows_the_zoom() {
    let (recipe, warnings) = convert_background(json!({
        "background-color": ["step", ["zoom"], "#ff0000", 10, "#0000ff"]
    }));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(recipe["nodes"]["bg__bg"]["color"], "@bg__color");
    assert_eq!(pixel_at(&recipe, 8), [255, 0, 0, 255]);
    assert_eq!(pixel_at(&recipe, 12), [0, 0, 255, 255]);
}

#[test]
fn opacity_and_a_translucent_colour_fold_into_the_alpha() {
    let (recipe, warnings) = convert_background(json!({
        "background-color": "rgba(0, 0, 255, 0.5)", "background-opacity": 0.5
    }));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(recipe["nodes"]["bg__bg"]["color"], "#0000ff40");

    let (recipe, _) = convert_background(json!({ "background-color": "#00ff00" }));
    assert_eq!(recipe["nodes"]["bg__bg"]["color"], "#00ff00");
}

#[test]
fn what_cannot_carry_over_warns() {
    let (_, warnings) = convert_background(json!({
        "background-color": "#00ff00",
        "background-opacity": ["interpolate", ["linear"], ["zoom"], 10, 0, 12, 1],
        "background-pattern": "dots"
    }));
    assert!(
        warnings.iter().any(|w| w.contains("background-opacity")),
        "{warnings:?}"
    );
    assert!(
        warnings.iter().any(|w| w.contains("background-pattern")),
        "{warnings:?}"
    );
}
