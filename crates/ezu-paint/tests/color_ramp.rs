//! `color-ramp` over a `ScalarField`. With no DEM asset bound, the
//! `dem` source node falls back to a zero-filled field — convenient
//! for testing the stop-table mapping without spinning up a fake
//! AssetLoader.

mod common;
use common::render;

#[test]
fn color_ramp_clamps_zero_field_to_first_stop() {
    // Unbound `dem` source emits an all-zero `ScalarField`. With the
    // first stop at value 0 (red) and a later stop at 100 (blue),
    // every pixel should map exactly to the first stop's colour.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "terrain": { "type": "dem",
                     "url": "http://example.invalid/{z}/{x}/{y}.webp",
                     "encoding": "terrarium" }
      },
      "nodes": {
        "dem":  { "op": "dem", "name": "tile.terrain" },
        "out":  { "op": "color-ramp", "field": "@dem",
                  "stops": [ { "value": 0,   "color": "#ff0000" },
                             { "value": 100, "color": "#0000ff" } ] }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    assert_eq!(p, [0xff, 0x00, 0x00, 0xff], "got {p:?}");
}

#[test]
fn color_ramp_below_range_clamps_to_first_stop() {
    // Zero field with stops at 100 (red) and 1000 (blue): 0 is below
    // the lowest stop, so every pixel clamps to the first colour.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "terrain": { "type": "dem",
                     "url": "http://example.invalid/{z}/{x}/{y}.webp",
                     "encoding": "terrarium" }
      },
      "nodes": {
        "dem":  { "op": "dem", "name": "tile.terrain" },
        "out":  { "op": "color-ramp", "field": "@dem",
                  "stops": [ { "value": 100,  "color": "#ff0000" },
                             { "value": 1000, "color": "#0000ff" } ] }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    assert_eq!(p, [0xff, 0x00, 0x00, 0xff], "got {p:?}");
}
