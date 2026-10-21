//! A raster layer's paint carries over: the colour properties onto the
//! `raster` node's fields, as numbers or through an `expr` node for a zoom
//! curve, and `raster-resampling` onto the source the node reads.

use ezu_graph::{
    build_graph, Cache, CanvasInfo, Evaluator, NoAssets, ParamValues, RasterBuf, TileId,
};
use ezu_paint::host::TileLoader;
use ezu_translate::maplibre::{convert, ConvertOptions};
use serde_json::{json, Value};

fn translate(layers: Value) -> Value {
    let style = json!({
        "version": 8,
        "sources": {
            "photo": { "type": "raster", "tiles": ["https://example.com/{z}/{x}/{y}.png"] }
        },
        "layers": layers
    });
    let (recipe, report) = convert(&style, &ConvertOptions::default()).expect("convert");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    recipe
}

#[test]
fn constant_paint_lands_on_the_raster_node() {
    let recipe = translate(json!([{
        "id": "sat", "type": "raster", "source": "photo",
        "paint": {
            "raster-opacity": 0.8, "raster-hue-rotate": 90,
            "raster-saturation": -0.5, "raster-contrast": 0.25,
            "raster-brightness-min": 0.1, "raster-brightness-max": 0.9
        }
    }]));
    let node = &recipe["nodes"]["sat__raster"];
    assert_eq!(node["op"], "raster");
    assert_eq!(node["source"], "photo");
    assert_eq!(node["opacity"], 0.8);
    assert_eq!(node["hue-rotate-deg"], 90.0);
    assert_eq!(node["saturation"], -0.5);
    assert_eq!(node["contrast"], 0.25);
    assert_eq!(node["brightness-min"], 0.1);
    assert_eq!(node["brightness-max"], 0.9);
}

#[test]
fn paint_at_its_defaults_leaves_the_node_bare() {
    let recipe = translate(json!([{
        "id": "sat", "type": "raster", "source": "photo",
        "paint": {
            "raster-opacity": 1, "raster-hue-rotate": 0, "raster-saturation": 0,
            "raster-contrast": 0, "raster-brightness-min": 0,
            "raster-brightness-max": 1, "raster-resampling": "linear"
        }
    }]));
    assert_eq!(
        recipe["nodes"]["sat__raster"],
        json!({ "op": "raster", "source": "photo" })
    );
    assert!(recipe["sources"]["photo"].get("resampling").is_none());
}

#[test]
fn a_zoom_curve_feeds_the_field_through_an_expr_node() {
    let curve = json!(["interpolate", ["linear"], ["zoom"], 5, 0, 10, 1]);
    let recipe = translate(json!([{
        "id": "sat", "type": "raster", "source": "photo",
        "paint": { "raster-opacity": curve }
    }]));
    let nodes = &recipe["nodes"];
    assert_eq!(nodes["sat__raster"]["opacity"], "@sat__opacity");
    assert_eq!(
        nodes["sat__opacity"],
        json!({ "op": "expr", "type": "number", "expr": curve })
    );

    // And it renders: transparent at z5, fully drawn from z10.
    let doc = ezu_style::Document::from_json(&serde_json::to_string(&recipe).unwrap()).unwrap();
    let graph = build_graph(&doc, &ezu_paint::nodes::default_registry()).unwrap();
    let cache = Cache::new();
    for (z, alpha) in [(5, 0), (10, 255)] {
        let tile = TileId { z, x: 0, y: 0 };
        let mut loader = TileLoader::new(&NoAssets, tile);
        let mut red = RasterBuf::new(8, 8);
        for px in red.pixels.chunks_mut(4) {
            px.copy_from_slice(&[255, 0, 0, 255]);
        }
        loader.bind_raster("photo", red);
        let out = Evaluator::new(&graph, &cache, &loader)
            .render(tile, CanvasInfo::square(8, 0), &ParamValues::new(), 0)
            .unwrap();
        let raster = out.as_raster().expect("a raster");
        assert_eq!(raster.pixels[3], alpha, "z{z}");
    }
}

#[test]
fn nearest_resampling_is_set_on_the_source() {
    let recipe = translate(json!([{
        "id": "sat", "type": "raster", "source": "photo",
        "paint": { "raster-resampling": "nearest" }
    }]));
    assert_eq!(recipe["sources"]["photo"]["resampling"], "nearest");
    assert_eq!(recipe["nodes"]["sat__raster"]["source"], "photo");
    // The recipe stays a valid document.
    ezu_style::Document::from_json(&serde_json::to_string(&recipe).unwrap()).unwrap();
}

#[test]
fn layers_that_disagree_on_resampling_read_separate_sources() {
    let recipe = translate(json!([
        { "id": "smooth", "type": "raster", "source": "photo" },
        { "id": "blocky", "type": "raster", "source": "photo",
          "paint": { "raster-resampling": "nearest" } },
        { "id": "blocky2", "type": "raster", "source": "photo",
          "paint": { "raster-resampling": "nearest" } }
    ]));
    let sources = &recipe["sources"];
    assert!(sources["photo"].get("resampling").is_none());
    assert_eq!(sources["photo__nearest"]["resampling"], "nearest");
    assert_eq!(sources["photo__nearest"]["url"], sources["photo"]["url"]);
    let nodes = &recipe["nodes"];
    assert_eq!(nodes["smooth__raster"]["source"], "photo");
    assert_eq!(nodes["blocky__raster"]["source"], "photo__nearest");
    assert_eq!(nodes["blocky2__raster"]["source"], "photo__nearest");

    // The other way round: the first layer's nearest stays on the source,
    // and the linear layer reads a copy without it.
    let recipe = translate(json!([
        { "id": "blocky", "type": "raster", "source": "photo",
          "paint": { "raster-resampling": "nearest" } },
        { "id": "smooth", "type": "raster", "source": "photo" }
    ]));
    assert_eq!(recipe["sources"]["photo"]["resampling"], "nearest");
    assert!(recipe["sources"]["photo__linear"]
        .get("resampling")
        .is_none());
    assert_eq!(recipe["nodes"]["smooth__raster"]["source"], "photo__linear");
}

#[test]
fn a_resampling_expression_warns_and_samples_linearly() {
    let style = json!({
        "version": 8,
        "sources": {
            "photo": { "type": "raster", "tiles": ["https://example.com/{z}/{x}/{y}.png"] }
        },
        "layers": [{ "id": "sat", "type": "raster", "source": "photo",
                     "paint": { "raster-resampling": ["step", ["zoom"], "linear", 10, "nearest"] } }]
    });
    let (recipe, report) = convert(&style, &ConvertOptions::default()).unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("raster-resampling")),
        "{:?}",
        report.warnings
    );
    assert!(recipe["sources"]["photo"].get("resampling").is_none());
}
