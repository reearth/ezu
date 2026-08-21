//! Smoke tests for utility ops: `switch`, `pick-channel`.

mod common;
use common::render;

#[test]
fn switch_default_picks_a() {
    // No `select` field → defaults to `a`. Red vs. blue solids:
    // confirm we see red.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "nodes": {
        "red":  { "op": "solid", "color": "#ff0000" },
        "blue": { "op": "solid", "color": "#0000ff" },
        "out":  { "op": "switch", "a": "@red", "b": "@blue" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    assert_eq!(r.pixel(4, 4), [0xff, 0x00, 0x00, 0xff]);
}

#[test]
fn switch_select_b_picks_b() {
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "nodes": {
        "red":  { "op": "solid", "color": "#ff0000" },
        "blue": { "op": "solid", "color": "#0000ff" },
        "out":  { "op": "switch", "a": "@red", "b": "@blue", "select": "b" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    assert_eq!(r.pixel(4, 4), [0x00, 0x00, 0xff, 0xff]);
}

#[test]
fn pick_channel_alpha_into_color_ramp() {
    // A red disk on a 32 canvas. `pick-channel a` extracts the
    // alpha as a ScalarField (0 outside the disk, 1 inside). Feed
    // through `color-ramp` mapping 0→green, 1→blue. The centre
    // (alpha=1) should render blue, the corner (alpha=0) green.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "disk":   { "op": "circle", "color": "#ff0000", "radius-frac": 0.4 },
        "alpha":  { "op": "pick-channel", "input": "@disk", "channel": "a" },
        "out":    { "op": "color-ramp", "field": "@alpha",
                    "stops": [ { "value": 0, "color": "#00ff00" },
                               { "value": 1, "color": "#0000ff" } ] }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    let centre = r.pixel(16, 16);
    assert_eq!(centre[2], 0xff, "centre alpha=1 → blue: {centre:?}");
    let corner = r.pixel(0, 0);
    assert_eq!(corner[1], 0xff, "corner alpha=0 → green: {corner:?}");
}

#[test]
fn pick_channel_luminance_of_pure_red_is_low() {
    // Rec. 601 luma of pure red: 0.299. Mapped through 0→red, 0.5→blue,
    // 1→green, the value 0.299 sits inside [0, 0.5] so we should be
    // on the red→blue half.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "nodes": {
        "src":  { "op": "solid", "color": "#ff0000" },
        "lum":  { "op": "pick-channel", "input": "@src", "channel": "luminance" },
        "out":  { "op": "color-ramp", "field": "@lum",
                  "stops": [ { "value": 0,   "color": "#ff0000" },
                             { "value": 0.5, "color": "#0000ff" },
                             { "value": 1.0, "color": "#00ff00" } ] }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    // 0.299 is 60% of the way from 0 to 0.5; expect predominantly
    // blue with some red mixed in.
    assert!(
        p[2] > p[1],
        "luminance ramp should land in blue half: {p:?}"
    );
}
