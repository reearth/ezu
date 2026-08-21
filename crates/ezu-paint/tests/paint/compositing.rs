//! Solid / circle / blur / blend variants — the core compositing pipeline.

mod common;
use common::render;

#[test]
fn solid_only_produces_uniform_raster() {
    let json = r##"{
      "name": "demo",
      "tile-size": 16,
      "nodes": { "bg": { "op": "solid", "color": "#3366ff" } },
      "output": "@bg"
    }"##;
    let r = render(json, 16, 0);
    assert_eq!(r.width, 16);
    let p = r.pixel(8, 8);
    assert_eq!(p, [0x33, 0x66, 0xff, 0xff]);
}

#[test]
fn circle_fill_then_blend_over_background() {
    // Background is opaque red. A blue disk drawn at center, blended on top.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "bg":    { "op": "solid", "color": "#ff0000" },
        "blue":  { "op": "circle", "color": "#0000ff", "radius-frac": 0.4 },
        "out":   { "op": "blend", "base": "@bg", "over": "@blue" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Center pixel should be blue (mask = 1).
    let center = r.pixel(16, 16);
    assert!(
        center[2] > 200,
        "center should be blue-dominant: {center:?}"
    );
    assert!(center[0] < 32, "center red should be near zero: {center:?}");
    // Corner pixel should be red (outside disk).
    let corner = r.pixel(0, 0);
    assert_eq!(corner, [0xff, 0x00, 0x00, 0xff]);
}

#[test]
fn blur_softens_disk_edge() {
    let json_sharp = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "fill":  { "op": "circle", "color": "#000000ff", "radius-frac": 0.4 }
      },
      "output": "@fill"
    }"##;
    let json_blur = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "disk":  { "op": "circle", "color": "#000000ff", "radius-frac": 0.4 },
        "fill":  { "op": "blur", "input": "@disk", "sigma": 1.5 }
      },
      "output": "@fill"
    }"##;
    let sharp = render(json_sharp, 32, 0);
    let blur = render(json_blur, 32, 0);
    // A pixel just outside the disk edge should be more covered by the
    // blurred version (alpha > 0) but transparent in the sharp one.
    // radius = 32 * 0.4 = 12.8 → check pixel at (16+13, 16) ≈ outside.
    let px_sharp = sharp.pixel(29, 16);
    let px_blur = blur.pixel(29, 16);
    assert_eq!(
        px_sharp[3], 0,
        "outside the disk, sharp version is transparent"
    );
    assert!(
        px_blur[3] > 0,
        "outside the disk, blurred version has some coverage: {px_blur:?}"
    );
}

#[test]
fn blend_multiply_darkens_base() {
    // Multiply two opaque mid-grays: 0x80 * 0x80 / 0xff ≈ 0x40.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "nodes": {
        "a":   { "op": "solid", "color": "#808080" },
        "b":   { "op": "solid", "color": "#808080" },
        "out": { "op": "blend", "base": "@a", "over": "@b", "mode": "multiply" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    // Expect close to 0x40 (64). Allow ±2 for rounding.
    assert!((p[0] as i32 - 0x40).abs() <= 2, "got {p:?}");
    assert_eq!(p[3], 0xff, "fully opaque");
}

#[test]
fn blend_clip_confines_to_base_alpha() {
    // base is a circle (alpha varies); over is solid red. With clip,
    // pixels outside the circle stay transparent.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "base": { "op": "circle", "color": "#0000ff", "radius-frac": 0.3 },
        "over": { "op": "solid", "color": "#ff0000" },
        "out":  { "op": "blend", "base": "@base", "over": "@over", "clip": true }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Corner is outside the disk → base alpha = 0 → clip output alpha = 0.
    let corner = r.pixel(0, 0);
    assert_eq!(
        corner[3], 0,
        "outside base alpha must be 0 under clip: {corner:?}"
    );
    // Center is inside disk → red shows through atop blue's alpha.
    let center = r.pixel(16, 16);
    assert!(center[3] > 200, "center should be opaque: {center:?}");
    assert!(center[0] > 200, "center should be red-dominant: {center:?}");
}

#[test]
fn blend_mask_modulates_over_coverage() {
    // mask is a small disk; using it as the mask input means red over
    // only appears where the mask is opaque.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "base": { "op": "solid", "color": "#0000ff" },
        "over": { "op": "solid", "color": "#ff0000" },
        "mask": { "op": "circle", "color": "#ffffff", "radius-frac": 0.3 },
        "out":  { "op": "blend", "base": "@base", "over": "@over", "mask": "@mask" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Outside mask → still pure blue.
    let corner = r.pixel(0, 0);
    assert_eq!(corner, [0x00, 0x00, 0xff, 0xff]);
    // Inside mask → red wins.
    let center = r.pixel(16, 16);
    assert!(center[0] > 200, "center should be red: {center:?}");
    assert!(
        center[2] < 32,
        "center blue should be near zero: {center:?}"
    );
}

#[test]
fn blend_destination_out_erases_base_under_over() {
    // base is opaque red everywhere; over is a centered disk. With
    // composite=destination-out, the disk-shaped region becomes
    // transparent, the rest stays red.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "base": { "op": "solid", "color": "#ff0000" },
        "over": { "op": "circle", "color": "#ffffff", "radius-frac": 0.4 },
        "out":  { "op": "blend", "base": "@base", "over": "@over", "composite": "destination-out" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    let center = r.pixel(16, 16);
    assert_eq!(center[3], 0, "center should be erased: {center:?}");
    let corner = r.pixel(0, 0);
    assert_eq!(
        corner,
        [0xff, 0x00, 0x00, 0xff],
        "corner untouched: {corner:?}"
    );
}
