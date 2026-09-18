//! Source resolution for the source nodes (`features`, `raster`,
//! `dem`): which `sources` entries each may target, and the warning
//! raised when a node leans on the single-source default instead of
//! naming what it reads.

use ezu_graph::build_graph;
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

/// Build the style and return the graph's build-time warnings.
fn build(json: &str) -> Result<Vec<String>, String> {
    let doc = Document::from_json(json).map_err(|e| e.to_string())?;
    build_graph(&doc, &default_registry())
        .map(|g| g.warnings().to_vec())
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
    let warnings = build(json).expect("a geojson source must be a valid features target");
    assert!(
        warnings.is_empty(),
        "a named source must not warn, got: {warnings:?}"
    );
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
    let warnings = build(json).expect("a lone geojson source must resolve as the default");
    assert_eq!(warnings.len(), 1, "got: {warnings:?}");
    let w = &warnings[0];
    assert!(w.contains("node `feat`"), "got: {w}");
    assert!(w.contains("`source` is not set"), "got: {w}");
    assert!(w.contains("`areas`"), "got: {w}");
}

#[test]
fn raster_warns_when_it_falls_back_to_the_only_source() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "sat": { "type": "raster", "url": "http://example.invalid/{z}/{x}/{y}.png" }
      },
      "nodes": {
        "out": { "op": "raster" }
      },
      "output": "@out"
    }"##;
    let warnings = build(json).expect("a lone raster source must resolve as the default");
    assert_eq!(warnings.len(), 1, "got: {warnings:?}");
    assert!(warnings[0].contains("node `out`"), "got: {}", warnings[0]);
    assert!(warnings[0].contains("`sat`"), "got: {}", warnings[0]);
}

#[test]
fn dem_warns_when_it_falls_back_to_the_only_source() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "terrain": { "type": "dem",
                     "url": "http://example.invalid/{z}/{x}/{y}.webp",
                     "encoding": "terrarium" }
      },
      "nodes": {
        "h":   { "op": "dem" },
        "out": { "op": "color-ramp", "field": "@h",
                 "stops": [ { "value": 0, "color": "#000000" },
                            { "value": 1, "color": "#ffffff" } ] }
      },
      "output": "@out"
    }"##;
    let warnings = build(json).expect("a lone dem source must resolve as the default");
    assert_eq!(warnings.len(), 1, "got: {warnings:?}");
    assert!(warnings[0].contains("node `h`"), "got: {}", warnings[0]);
    assert!(warnings[0].contains("`terrain`"), "got: {}", warnings[0]);
}

#[test]
fn naming_the_source_silences_the_warning() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "sat": { "type": "raster", "url": "http://example.invalid/{z}/{x}/{y}.png" }
      },
      "nodes": {
        "out": { "op": "raster", "source": "sat" }
      },
      "output": "@out"
    }"##;
    let warnings = build(json).expect("builds");
    assert!(warnings.is_empty(), "got: {warnings:?}");
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
        err.contains("not a `mvt`/`pmtiles`/`geojson` source"),
        "got: {err}"
    );
}
