//! The `raster` source node: host-bound RGBA imagery as a `Raster`,
//! transparent fallback when unbound, and downstream filtering; and the
//! zoom band on both `raster` and `dem`.

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

#[test]
fn raster_node_draws_nothing_outside_its_zoom_band() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "photo": { "type": "raster",
                   "url": "http://example.invalid/{z}/{x}/{y}.jpg" }
      },
      "nodes": { "out": { "op": "raster", "min-zoom": 3, "max-zoom": 5 } },
      "output": "@out"
    }"##;
    let green = || [("photo", solid_sprite(8, 8, [0, 64, 0, 255]))];
    for (z, drawn) in [(2, false), (3, true), (5, true), (6, false)] {
        let tile = TileId { z, x: 0, y: 0 };
        let r = render_with_rasters(json, 8, 0, tile, &green());
        let want = if drawn { [0, 64, 0, 255] } else { [0; 4] };
        assert_eq!(r.pixel(4, 4), want, "z{z}");
    }
}

#[test]
fn dem_node_yields_a_zero_field_outside_its_zoom_band() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "terrain": { "type": "dem", "encoding": "terrarium",
                     "url": "http://example.invalid/{z}/{x}/{y}.png" }
      },
      "nodes": {
        "dem": { "op": "dem", "min-zoom": 3, "max-zoom": 5 },
        "out": { "op": "color-ramp", "field": "@dem",
                 "stops": [ { "value": 0, "color": "#000000" },
                            { "value": 100, "color": "#ffffff" } ] }
      },
      "output": "@out"
    }"##;
    let hill = || ezu_graph::ScalarField {
        width: 8,
        height: 8,
        values: vec![100.0; 64].into(),
        nodata: None,
        geo_scale: None,
    };
    for (z, high) in [(2, false), (3, true), (5, true), (6, false)] {
        let tile = TileId { z, x: 0, y: 0 };
        let f = crate::common::field_of_node(json, 8, 0, tile, &[("terrain", hill())], "dem");
        assert_eq!(f.values[0], if high { 100.0 } else { 0.0 }, "z{z}");
    }
}
