//! The `raster` source node: host-bound RGBA imagery as a `Raster`,
//! transparent fallback when unbound, and downstream filtering.

use crate::common::{render_with_rasters, solid_sprite};

use ezu_graph::TileId;

const Z0: TileId = TileId { z: 0, x: 0, y: 0 };

#[test]
fn raster_node_passes_bound_imagery_through() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "photo": { "type": "raster",
                   "url": "http://example.invalid/{z}/{x}/{y}.jpg",
                   "attribution": "© Example Sat" }
      },
      "nodes": { "out": { "op": "raster" } },
      "output": "@out"
    }"##;
    // The test loader binds by bare source name, standing in for the
    // host's per-tile stitch + bind.
    let r = render_with_rasters(
        json,
        8,
        0,
        Z0,
        &[("photo", solid_sprite(8, 8, [0, 64, 0, 255]))],
    );
    assert_eq!(r.pixel(4, 4), [0, 64, 0, 255]);
}

#[test]
fn unbound_raster_source_is_transparent() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "photo": { "type": "raster",
                   "url": "http://example.invalid/{z}/{x}/{y}.jpg" }
      },
      "nodes": {
        "bg":    { "op": "solid", "color": "#102030" },
        "photo": { "op": "raster", "source": "photo" },
        "out":   { "op": "blend", "base": "@bg", "over": "@photo" }
      },
      "output": "@out"
    }"##;
    // No binding -> the raster node emits transparent pixels; the
    // background shows through unchanged.
    let r = crate::common::render(json, 8, 0);
    assert_eq!(r.pixel(4, 4), [0x10, 0x20, 0x30, 0xff]);
}

#[test]
fn raster_feeds_downstream_filters() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "photo": { "type": "raster",
                   "url": "http://example.invalid/{z}/{x}/{y}.jpg" }
      },
      "nodes": {
        "photo": { "op": "raster" },
        "out":   { "op": "invert", "input": "@photo" }
      },
      "output": "@out"
    }"##;
    let r = render_with_rasters(
        json,
        8,
        0,
        Z0,
        &[("photo", solid_sprite(8, 8, [255, 255, 255, 255]))],
    );
    // Inverted white -> black (alpha preserved).
    let p = r.pixel(4, 4);
    assert_eq!(p[3], 0xff);
    assert!(p[0] < 8 && p[1] < 8 && p[2] < 8, "got {p:?}");
}
