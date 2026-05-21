//! Color-adjustment nodes: invert, brightness-contrast, hsl, color-to-alpha.

mod common;
use common::render;

#[test]
fn invert_negates_rgb_preserving_alpha() {
    let json = r##"{
      "name": "demo",
      "tile-size": 4,
      "nodes": {
        "src": { "op": "solid", "color": "#204060" },
        "out": { "op": "invert", "input": "@src" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 4, 0);
    // 0x20 -> 0xdf, 0x40 -> 0xbf, 0x60 -> 0x9f.
    assert_eq!(r.pixel(0, 0), [0xdf, 0xbf, 0x9f, 0xff]);
}

#[test]
fn brightness_contrast_shifts_levels() {
    let json = r##"{
      "name": "demo",
      "tile-size": 4,
      "nodes": {
        "src": { "op": "solid", "color": "#808080" },
        "out": { "op": "brightness-contrast", "input": "@src", "brightness": 0.25 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 4, 0);
    // 0.5 + 0.25 = 0.75 -> ~0xbf.
    let p = r.pixel(0, 0);
    assert!((p[0] as i32 - 0xbf).abs() <= 2, "got {p:?}");
}

#[test]
fn hsl_hue_shift_rotates_color() {
    // Pure red rotated by +120° -> pure green.
    let json = r##"{
      "name": "demo",
      "tile-size": 4,
      "nodes": {
        "src": { "op": "solid", "color": "#ff0000" },
        "out": { "op": "hsl", "input": "@src", "hue-shift": 120 }
      },
      "output": "@out"
    }"##;
    let r = render(json, 4, 0);
    let p = r.pixel(0, 0);
    assert!(p[1] > 240 && p[0] < 8 && p[2] < 8, "expected pure green: {p:?}");
}

#[test]
fn color_to_alpha_keys_out_target() {
    // Red surface: keying red drops alpha to 0, keying blue leaves it.
    let json_keyed = r##"{
      "name": "demo",
      "tile-size": 4,
      "nodes": {
        "src": { "op": "solid", "color": "#ff0000" },
        "out": { "op": "color-to-alpha", "input": "@src", "color": "#ff0000", "threshold": 0.05, "softness": 0.05 }
      },
      "output": "@out"
    }"##;
    let json_kept = r##"{
      "name": "demo",
      "tile-size": 4,
      "nodes": {
        "src": { "op": "solid", "color": "#ff0000" },
        "out": { "op": "color-to-alpha", "input": "@src", "color": "#0000ff", "threshold": 0.05, "softness": 0.05 }
      },
      "output": "@out"
    }"##;
    let keyed = render(json_keyed, 4, 0);
    let kept = render(json_kept, 4, 0);
    assert_eq!(keyed.pixel(0, 0)[3], 0, "matching color should be transparent");
    assert_eq!(kept.pixel(0, 0)[3], 0xff, "distant color should be opaque");
}
