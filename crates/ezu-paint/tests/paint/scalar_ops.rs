//! Smoke tests for the `ScalarField` math ops: `map-range`,
//! `threshold`. Both are validated by piping through `color-ramp` so
//! the test asserts on rendered pixel colour.

mod common;
use common::render;

#[test]
fn map_range_normalises_zero_field_via_color_ramp() {
    // Unbound DEM source emits an all-zero ScalarField. Remap that
    // through [-1000, 1000] -> [0, 1] (zero lands at 0.5) and feed
    // into color-ramp; 0.5 is exactly between the red and blue
    // stops, so the result should be middling purple.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "terrain": { "type": "dem",
                     "url": "http://example.invalid/{z}/{x}/{y}.webp",
                     "encoding": "terrarium" }
      },
      "nodes": {
        "dem":   { "op": "dem", "name": "tile.terrain" },
        "norm":  { "op": "map-range", "field": "@dem",
                   "in-min": -1000, "in-max": 1000,
                   "out-min": 0, "out-max": 1, "clamp": true },
        "out":   { "op": "color-ramp", "field": "@norm",
                   "stops": [ { "value": 0, "color": "#ff0000" },
                              { "value": 1, "color": "#0000ff" } ] }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    // Halfway between red and blue: equal R and B, both around 0x80.
    assert!(
        (p[0] as i32 - 0x80).abs() < 8 && (p[2] as i32 - 0x80).abs() < 8,
        "expected mid-purple, got {p:?}"
    );
}

#[test]
fn threshold_binarises_via_color_ramp() {
    // Zero field thresholded at -100 with hard step: every pixel is
    // above -100, so output is `high` (1.0). Map 1.0 through a ramp
    // [0=red, 1=blue]; expect blue.
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
        "bin":  { "op": "threshold", "field": "@dem", "value": -100 },
        "out":  { "op": "color-ramp", "field": "@bin",
                  "stops": [ { "value": 0, "color": "#ff0000" },
                             { "value": 1, "color": "#0000ff" } ] }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    assert_eq!(p, [0x00, 0x00, 0xff, 0xff], "got {p:?}");
}
