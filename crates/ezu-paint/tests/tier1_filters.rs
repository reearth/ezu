//! Smoke tests for the Tier 1 scalar / raster filter additions:
//! `map-range`, `threshold`, `levels`, `erode`, `dilate`, `edge-detect`.

mod common;
use common::render;

// ----- scalar ops -----

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

// ----- raster ops -----

#[test]
fn levels_with_gamma_lifts_midtones() {
    // 50% gray, lifted by gamma=2.2 → noticeably brighter (~0xb4).
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "nodes": {
        "base": { "op": "solid", "color": "#808080" },
        "out":  { "op": "levels", "input": "@base", "gamma": 2.2 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    assert!(
        p[0] > 0xa0 && p[0] < 0xd0,
        "expected lifted gray, got {p:?}"
    );
    assert_eq!(p[3], 0xff);
}

#[test]
fn erode_shrinks_disk_alpha() {
    // A radius-frac 0.4 disk on a 32 canvas, eroded by 4 px. A pixel
    // just inside the original edge (e.g. (28, 16)) should be much
    // less covered after erosion.
    let json_disk = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": { "out": { "op": "circle", "color": "#ff0000", "radius-frac": 0.4 } },
      "output": "@out"
    }"##;
    let json_eroded = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "disk": { "op": "circle", "color": "#ff0000", "radius-frac": 0.4 },
        "out":  { "op": "erode", "input": "@disk", "radius-px": 4 }
      },
      "output": "@out"
    }"##;
    let plain = render(json_disk, 32, 0);
    let eroded = render(json_eroded, 32, 0);
    // Centre stays opaque after a 4-px erode (disk radius is ~12).
    assert_eq!(eroded.pixel(16, 16)[3], 0xff);
    // Near the original edge, coverage should drop.
    let p_plain = plain.pixel(27, 16);
    let p_eroded = eroded.pixel(27, 16);
    assert!(
        p_eroded[3] < p_plain[3],
        "erode should reduce edge coverage: plain={p_plain:?} eroded={p_eroded:?}"
    );
}

#[test]
fn dilate_grows_disk_alpha() {
    let json_disk = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": { "out": { "op": "circle", "color": "#ff0000", "radius-frac": 0.3 } },
      "output": "@out"
    }"##;
    let json_dilated = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "disk": { "op": "circle", "color": "#ff0000", "radius-frac": 0.3 },
        "out":  { "op": "dilate", "input": "@disk", "radius-px": 4 }
      },
      "output": "@out"
    }"##;
    let plain = render(json_disk, 32, 0);
    let dilated = render(json_dilated, 32, 0);
    // Outside the original disk, dilated version has coverage.
    let p_plain = plain.pixel(27, 16);
    let p_dilated = dilated.pixel(27, 16);
    assert!(
        p_dilated[3] > p_plain[3],
        "dilate should grow edge coverage: plain={p_plain:?} dilated={p_dilated:?}"
    );
}

#[test]
fn edge_detect_highlights_disk_rim() {
    // The inside and outside of a disk are uniform; the gradient is
    // only non-zero on the rim. A pixel right at the edge should show
    // a strong gradient response, the centre essentially none.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "disk": { "op": "circle", "color": "#ff0000", "radius-frac": 0.4 },
        "out":  { "op": "edge-detect", "input": "@disk", "strength": 1.0 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    let centre = r.pixel(16, 16);
    let rim = r.pixel(28, 16);
    assert!(rim[3] > 32, "rim should have a gradient response: {rim:?}");
    assert!(
        centre[3] < 8,
        "centre should be near-zero gradient: {centre:?}"
    );
}
