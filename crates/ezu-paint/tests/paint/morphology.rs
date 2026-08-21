//! Smoke tests for raster morphology / edge ops: `erode`, `dilate`,
//! `edge-detect`. All operate as pass-through filters over
//! `Raster|Sprite`.

use crate::common::render;

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
