//! Vector-driven paint nodes: brush-solid + line, dash, wave, stamp.

mod common;
use common::render;

#[test]
fn brush_solid_line_paints_a_visible_stroke() {
    // brush-solid + line: draw a horizontal red line across the tile at y=mid.
    // `extent` 4096 covers the 32-px canvas; `y = 2048` is the middle row.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "feats": { "op": "literal-geometry", "extent": 4096,
                   "lines": [ [ [0, 2048], [4095, 2048] ] ] },
        "brush": { "op": "brush-solid", "width-px": 3, "color": "#ff0000" },
        "out":   { "op": "line", "features": "@feats", "brush": "@brush", "color": "#ff0000" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Center pixel of the middle row should have a strong red component.
    let mid = r.pixel(16, 16);
    assert!(mid[0] > 200, "center stroke should be red-dominant: {mid:?}");
    assert!(mid[3] > 200, "center stroke should be opaque: {mid:?}");
    // A pixel well above the stroke should be transparent.
    let above = r.pixel(16, 4);
    assert_eq!(above[3], 0, "above the stroke should be transparent: {above:?}");
}

#[test]
fn dash_chops_a_long_line_into_multiple_runs() {
    // A horizontal line dashed at 4-px dash / 4-px gap should leave the
    // tile striped: some columns are inked, some are clear.
    let json = r##"{
      "name": "demo",
      "tile-size": 64,
      "nodes": {
        "feats":  { "op": "literal-geometry", "extent": 4096,
                    "lines": [ [ [0, 2048], [4095, 2048] ] ] },
        "dashed": { "op": "dash", "features": "@feats",
                    "dash-px": 4, "gap-px": 4 },
        "brush":  { "op": "brush-solid", "width-px": 2, "color": "#000000" },
        "out":    { "op": "line", "features": "@dashed", "brush": "@brush", "color": "#000000" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 64, 0);
    // Across the middle row, sample alpha. Expect alternation: some
    // columns hit a dash (alpha > 0), others fall in a gap (alpha = 0).
    let mut inked = 0;
    let mut clear = 0;
    for x in 0..64 {
        let a = r.pixel(x, 32)[3];
        if a > 32 {
            inked += 1;
        } else if a < 8 {
            clear += 1;
        }
    }
    assert!(inked > 8 && clear > 8, "expected stripes: inked={inked} clear={clear}");
}

#[test]
fn wave_lifts_a_horizontal_line_off_its_baseline() {
    // A horizontal source line should, after wave displacement, leave
    // pixels above and below the baseline row.
    let json = r##"{
      "name": "demo",
      "tile-size": 64,
      "nodes": {
        "feats":  { "op": "literal-geometry", "extent": 4096,
                    "lines": [ [ [0, 2048], [4095, 2048] ] ] },
        "wavy":   { "op": "wave", "features": "@feats",
                    "amplitude-px": 10, "wavelength-px": 20 },
        "brush":  { "op": "brush-solid", "width-px": 2, "color": "#000000" },
        "out":    { "op": "line", "features": "@wavy", "brush": "@brush", "color": "#000000" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 64, 0);
    // Sample rows within the wave envelope on both sides of the
    // baseline (y=32). With amplitude 10 px, the curve reaches roughly
    // y=22 (above) and y=42 (below); sampling y=28 / y=36 stays well
    // inside the inked envelope but is firmly off the baseline.
    let mut above = false;
    let mut below = false;
    for x in 8..56 {
        if r.pixel(x, 28)[3] > 32 {
            above = true;
        }
        if r.pixel(x, 36)[3] > 32 {
            below = true;
        }
    }
    assert!(above, "wave should push pixels above the baseline");
    assert!(below, "wave should push pixels below the baseline");
}

#[test]
fn stamp_places_image_at_each_point() {
    // Use `circle` as a sprite (canvas-sized, but only the inner disk is
    // opaque). Stamping at two extent positions should leave two visible
    // splotches. Stamp draws the full sprite at each point; since the
    // sprite is canvas-sized, both stamps overlap heavily — we only
    // check that the output has substantial coverage and isn't blank.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "feats": { "op": "literal-geometry", "extent": 4096,
                   "points": [ [1024, 2048], [3072, 2048] ] },
        "img":   { "op": "circle", "color": "#00ff00", "radius-frac": 0.15 },
        "out":   { "op": "stamp", "features": "@feats", "image": "@img", "scale": 1.0 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    let mut green = 0;
    for y in 0..32 {
        for x in 0..32 {
            let p = r.pixel(x, y);
            if p[1] > 100 && p[3] > 100 {
                green += 1;
            }
        }
    }
    assert!(green > 4, "stamp should leave visible green pixels: got {green}");
}
