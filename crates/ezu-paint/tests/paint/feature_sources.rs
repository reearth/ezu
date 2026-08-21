//! `features` source resolution: which `sources` entries a `features`
//! node may target, explicitly or by single-source default.

use ezu_graph::build_graph;
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

fn build(json: &str) -> Result<(), String> {
    let doc = Document::from_json(json).map_err(|e| e.to_string())?;
    build_graph(&doc, &default_registry())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[test]
fn features_accepts_a_geojson_source() {
    // A translated MapLibre style may draw a layer straight from a
    // `geojson` source (e.g. demotiles' `crimea`); the host binds it as
    // `<source>.<source>`, so the node must accept it like mvt/pmtiles.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "areas": { "type": "geojson",
                   "data": { "type": "FeatureCollection", "features": [] } }
      },
      "nodes": {
        "feat": { "op": "features", "source": "areas", "layer": "areas" },
        "out":  { "op": "fill-solid", "features": "@feat", "fill": "#336699" }
      },
      "output": "@out"
    }"##;
    build(json).expect("a geojson source must be a valid features target");
}

#[test]
fn features_defaults_to_a_single_geojson_source() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "areas": { "type": "geojson",
                   "data": { "type": "FeatureCollection", "features": [] } }
      },
      "nodes": {
        "feat": { "op": "features", "layer": "areas" },
        "out":  { "op": "fill-solid", "features": "@feat", "fill": "#336699" }
      },
      "output": "@out"
    }"##;
    build(json).expect("a lone geojson source must resolve as the default");
}

#[test]
fn features_still_rejects_non_feature_sources() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "terrain": { "type": "dem",
                     "url": "http://example.invalid/{z}/{x}/{y}.webp",
                     "encoding": "terrarium" }
      },
      "nodes": {
        "feat": { "op": "features", "source": "terrain", "layer": "x" },
        "out":  { "op": "fill-solid", "features": "@feat", "fill": "#336699" }
      },
      "output": "@out"
    }"##;
    let err = build(json).expect_err("a dem source must be rejected");
    assert!(
        err.contains("not an `mvt` / `pmtiles` / `geojson` source"),
        "got: {err}"
    );
}
