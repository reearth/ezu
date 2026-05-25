//! Smoke tests for the Tier 2 raster filter additions:
//! `posterize`, `channel-shuffle`, `sharpen`.

mod common;
use common::render;

#[test]
fn posterize_snaps_midtone_to_quantised_level() {
    // 50% gray, posterised to 2 steps (just black and white): every
    // pixel snaps to one of the endpoints.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "nodes": {
        "base": { "op": "solid", "color": "#808080" },
        "out":  { "op": "posterize", "input": "@base", "steps": 2 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    // 0x80 / 255 = 0.502; rounds to 1 → white.
    assert_eq!(p, [0xff, 0xff, 0xff, 0xff], "got {p:?}");
}

#[test]
fn posterize_preserves_endpoints() {
    // Pure red → pure red regardless of step count.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "nodes": {
        "base": { "op": "solid", "color": "#ff0000" },
        "out":  { "op": "posterize", "input": "@base", "steps": 4 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    assert_eq!(r.pixel(4, 4), [0xff, 0x00, 0x00, 0xff]);
}

#[test]
fn channel_shuffle_swaps_red_and_blue() {
    // Red input swapped to blue via r←b, b←r.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "nodes": {
        "base": { "op": "solid", "color": "#ff0000" },
        "out":  { "op": "channel-shuffle", "input": "@base",
                  "r": "b", "b": "r" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    assert_eq!(r.pixel(4, 4), [0x00, 0x00, 0xff, 0xff]);
}

#[test]
fn channel_shuffle_constant_one_fills_channel() {
    // Black input, force green = 1.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "nodes": {
        "base": { "op": "solid", "color": "#000000" },
        "out":  { "op": "channel-shuffle", "input": "@base", "g": "1" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    assert_eq!(p[1], 0xff, "green should be saturated: {p:?}");
    assert_eq!(p[0], 0x00);
    assert_eq!(p[2], 0x00);
}

#[test]
fn sharpen_amplifies_edge_contrast() {
    // A disk edge: sharpen should make the dark side darker and the
    // bright side brighter at the rim, compared to the plain disk.
    let json_plain = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "bg":   { "op": "solid",  "color": "#ffffff" },
        "disk": { "op": "circle", "color": "#000000", "radius-frac": 0.4 },
        "out":  { "op": "blend",  "base": "@bg", "over": "@disk" }
      },
      "output": "@out"
    }"##;
    let json_sharp = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "bg":    { "op": "solid",   "color": "#ffffff" },
        "disk":  { "op": "circle",  "color": "#000000", "radius-frac": 0.4 },
        "base":  { "op": "blend",   "base": "@bg", "over": "@disk" },
        "out":   { "op": "sharpen", "input": "@base", "amount": 1.0 }
      },
      "output": "@out"
    }"##;
    let plain = render(json_plain, 32, 0);
    let sharp = render(json_sharp, 32, 0);
    // Just outside the disk (≈ (29, 16)): plain shows the white
    // background trending toward gray as we approach the rim;
    // sharpen pushes it back toward white (overshoots).
    let p_plain = plain.pixel(29, 16);
    let p_sharp = sharp.pixel(29, 16);
    assert!(
        p_sharp[0] >= p_plain[0],
        "outside rim should stay ≥ as bright after sharpen: plain={p_plain:?} sharp={p_sharp:?}"
    );
}
