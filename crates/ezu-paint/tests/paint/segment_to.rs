//! `segment-to`: connectors from a position each feature names in
//! longitude and latitude to the feature's point, projected into the tile
//! being rendered.

use crate::common::render_with_features;
use ezu_core::coord::{world_x_to_lon, world_y_to_lat};
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

const TILE: TileId = TileId {
    z: 17,
    x: 116_420,
    y: 51_610,
};
const EXTENT: f64 = 4096.0;

/// The longitude and latitude of tile position `(x, y)`.
fn lng_lat(x: f64, y: f64) -> (f64, f64) {
    let n = f64::from(1u32 << TILE.z);
    (
        world_x_to_lon((f64::from(TILE.x) + x / EXTENT) / n),
        world_y_to_lat((f64::from(TILE.y) + y / EXTENT) / n),
    )
}

/// A point at `at` whose other end is tile position `to`, with only the
/// fields named in `fields` set.
fn point(at: (i32, i32), to: (f64, f64), fields: &[&str]) -> Feature {
    let (lng, lat) = lng_lat(to.0, to.1);
    let mut properties = HashMap::new();
    for &f in fields {
        let v = if f == "rep_lng" { lng } else { lat };
        properties.insert(f.to_string(), Value::Double(v));
    }
    let mut geometry = Geometry::default();
    geometry.points.push(at);
    Feature {
        id: None,
        geometry,
        properties,
    }
}

#[test]
fn connectors_run_from_the_named_position_to_each_point() {
    let both = ["rep_lng", "rep_lat"];
    let layer = FeatureLayer {
        name: "entrances".to_string(),
        extent: 4096,
        features: vec![
            // Across the middle, from px (16, 32) to (48, 32).
            point((3072, 2048), (1024.0, 2048.0), &both),
            // Down the middle, but its latitude is missing.
            point((2048, 3584), (2048.0, 512.0), &["rep_lng"]),
            // From px (16, 16) to a point far east of the tile, as a
            // stitched neighbour supplies it.
            point((6000, 1024), (1024.0, 1024.0), &both),
            // Its other end is the point itself.
            point((1024, 3584), (1024.0, 3584.0), &both),
        ],
    };
    let json = r##"{
      "name": "connectors",
      "tile-size": 64,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "bg":    { "op": "solid", "color": "#ffffff" },
        "pts":   { "op": "features", "source": "src", "layer": "entrances" },
        "links": { "op": "segment-to", "features": "@pts",
                   "lng-field": "rep_lng", "lat-field": "rep_lat" },
        "draw":  { "op": "stroke", "features": "@links", "color": "#000000", "width-px": 2 },
        "out":   { "op": "blend", "base": "@bg", "over": "@draw" }
      },
      "output": "@out"
    }"##;
    let r = render_with_features(json, 64, 0, TILE, &[("src.entrances", layer)]);
    let dark = |x: u32, y: u32| r.pixel(x, y)[0] < 128;
    assert!(dark(20, 32) && dark(44, 32), "connector across the middle");
    assert!(!dark(10, 32) && !dark(54, 32), "it stops at its two ends");
    assert!(
        !dark(32, 20) && !dark(32, 44),
        "no connector without a latitude"
    );
    assert!(
        dark(40, 16) && dark(62, 16),
        "connector from outside the tile"
    );
    assert!(!dark(10, 16), "starting at its named end");
}
