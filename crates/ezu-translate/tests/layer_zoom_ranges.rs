//! `background`, `raster` and `hillshade` layers keep their `minzoom` /
//! `maxzoom`, as feature layers always have: the translated recipe draws
//! them only inside that band.

use ezu_graph::{build_graph, Cache, CanvasInfo, Evaluator, NoAssets, ParamValues, TileId};
use ezu_paint::host::TileLoader;
use ezu_translate::maplibre::{convert, ConvertOptions};
use serde_json::{json, Value};

fn translate(layers: Value) -> Value {
    let style = json!({
        "version": 8,
        "sources": {
            "photo": { "type": "raster", "tiles": ["https://example.com/{z}/{x}/{y}.png"] },
            "terrain": { "type": "raster-dem", "tiles": ["https://example.com/dem/{z}/{x}/{y}.png"] }
        },
        "layers": layers
    });
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("convert");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    recipe
}

fn node<'a>(recipe: &'a Value, op: &str) -> &'a Value {
    recipe["nodes"]
        .as_object()
        .unwrap()
        .values()
        .find(|n| n["op"] == op)
        .unwrap_or_else(|| panic!("a `{op}` node"))
}

#[test]
fn raster_and_hillshade_layers_gate_their_source_node() {
    // MapLibre draws `minzoom <= z < maxzoom`; ezu's gate is inclusive.
    let recipe = translate(json!([
        { "id": "sat", "type": "raster", "source": "photo", "minzoom": 3, "maxzoom": 9 },
        { "id": "hills", "type": "hillshade", "source": "terrain", "minzoom": 6 }
    ]));
    let raster = node(&recipe, "raster");
    assert_eq!(raster["min-zoom"], 3);
    assert_eq!(raster["max-zoom"], 8);
    let dem = node(&recipe, "dem");
    assert_eq!(dem["min-zoom"], 6);
    assert!(dem.get("max-zoom").is_none());
}

#[test]
fn layers_without_a_band_have_no_gate() {
    let recipe = translate(json!([
        { "id": "bg", "type": "background", "paint": { "background-color": "#ff0000" } },
        { "id": "sat", "type": "raster", "source": "photo" }
    ]));
    let raster = node(&recipe, "raster");
    assert!(raster.get("min-zoom").is_none() && raster.get("max-zoom").is_none());
    let ops: Vec<&str> = recipe["nodes"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(|n| n["op"].as_str())
        .collect();
    assert!(
        !ops.contains(&"switch") && !ops.contains(&"expr"),
        "{ops:?}"
    );
}

/// Which of `zooms` a background with `band` (its `minzoom`/`maxzoom`
/// fields) draws at.
fn background_drawn(band: Value, zooms: &[(u8, bool)]) {
    let mut layer = json!({ "id": "bg", "type": "background",
                            "paint": { "background-color": "#ff0000" } });
    layer
        .as_object_mut()
        .unwrap()
        .extend(band.as_object().unwrap().clone());
    let recipe = translate(json!([layer]));
    let doc = ezu_style::Document::from_json(&serde_json::to_string(&recipe).unwrap()).unwrap();
    let graph = build_graph(&doc, &ezu_paint::nodes::default_registry()).unwrap();
    let cache = Cache::new();
    for &(z, drawn) in zooms {
        let tile = TileId { z, x: 0, y: 0 };
        let loader = TileLoader::new(&NoAssets, tile);
        let out = Evaluator::new(&graph, &cache, &loader)
            .render(tile, CanvasInfo::square(8, 0), &ParamValues::new(), 0)
            .unwrap();
        let raster = out.as_raster().expect("a raster");
        let want = if drawn { [255, 0, 0, 255] } else { [0; 4] };
        assert_eq!(raster.pixels[..4], want, "z{z}");
    }
}

#[test]
fn a_background_draws_only_inside_its_band() {
    let band = [(3, false), (4, true), (6, true), (7, false)];
    background_drawn(json!({ "minzoom": 4, "maxzoom": 7 }), &band);
    background_drawn(
        json!({ "minzoom": 4 }),
        &[(3, false), (4, true), (20, true)],
    );
    background_drawn(json!({ "maxzoom": 7 }), &[(0, true), (6, true), (7, false)]);
}
