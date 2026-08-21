//! Rendering onto a canvas whose axes differ.
//!
//! Every render used to be a square map tile, and a good deal of code
//! took one number for the canvas and used it on both axes. A legend
//! swatch is the case that breaks that: its shape is whatever the legend
//! has room for. These tests are the ones that fail if any of those
//! places goes back to assuming one number.

use crate::common::render_shaped;

const W: u32 = 96;
const H: u32 = 32;

fn is_dark(px: [u8; 4]) -> bool {
    px[0] < 128 && px[1] < 128 && px[2] < 128
}

#[test]
fn the_output_has_the_shape_that_was_asked_for() {
    let json = r##"{
      "name": "rect",
      "nodes": { "out": { "op": "solid", "color": "#112233" } },
      "output": "@out"
    }"##;
    let r = render_shaped(json, W, H, 0);
    assert_eq!((r.width, r.height), (W, H));
    // Filled to all four corners, not to a square inscribed in it.
    for (x, y) in [(0, 0), (W - 1, 0), (0, H - 1), (W - 1, H - 1)] {
        assert!(is_dark(r.pixel(x, y)), "corner ({x}, {y}) unpainted");
    }
}

#[test]
fn padding_surrounds_both_axes() {
    let json = r##"{
      "name": "rect",
      "nodes": { "out": { "op": "solid", "color": "#112233" } },
      "output": "@out"
    }"##;
    let pad = 4;
    let r = render_shaped(json, W, H, pad);
    assert_eq!((r.width, r.height), (W + 2 * pad, H + 2 * pad));
}

/// A polygon covering the whole coordinate extent has to reach every
/// corner. It only does if the extent-to-pixel scale is taken per axis;
/// one scale for both leaves a band unpainted along the long side.
#[test]
fn a_full_extent_polygon_covers_the_whole_canvas() {
    let json = r##"{
      "name": "rect",
      "nodes": {
        "bg":   { "op": "solid", "color": "#ffffff" },
        "poly": { "op": "literal-geometry",
                  "polygons": [{ "exterior": [[0, 0], [4096, 0], [4096, 4096], [0, 4096]] }] },
        "fill": { "op": "fill-solid", "features": "@poly", "fill": "#111111" },
        "out":  { "op": "blend", "base": "@bg", "over": "@fill" }
      },
      "output": "@out"
    }"##;
    let r = render_shaped(json, W, H, 0);
    for (x, y) in [
        (1, 1),
        (W / 2, H / 2),
        (W - 2, 1),
        (1, H - 2),
        (W - 2, H - 2),
    ] {
        assert!(
            is_dark(r.pixel(x, y)),
            "({x}, {y}) left unpainted: {:?}",
            r.pixel(x, y)
        );
    }
}

/// A line across the middle of the extent has to land across the middle
/// of the canvas, not a third of the way down it.
#[test]
fn geometry_maps_to_the_canvas_centre_on_both_axes() {
    let json = r##"{
      "name": "rect",
      "nodes": {
        "bg":   { "op": "solid", "color": "#ffffff" },
        "line": { "op": "literal-geometry", "lines": [[[0, 2048], [4096, 2048]]] },
        "draw": { "op": "stroke", "features": "@line", "width-px": 3, "color": "#000000" },
        "out":  { "op": "blend", "base": "@bg", "over": "@draw" }
      },
      "output": "@out"
    }"##;
    let r = render_shaped(json, W, H, 0);
    let dark_rows: Vec<u32> = (0..H).filter(|&y| is_dark(r.pixel(W / 2, y))).collect();
    assert!(!dark_rows.is_empty(), "the line was not drawn");
    let centre = dark_rows.iter().sum::<u32>() / dark_rows.len() as u32;
    assert!(
        centre.abs_diff(H / 2) <= 2,
        "line centred on row {centre}, expected about {}",
        H / 2
    );
    // And it spans the full width.
    assert!(is_dark(r.pixel(1, centre)) && is_dark(r.pixel(W - 2, centre)));
}

/// A tile-anchored gradient spans `[0, 1]` across the canvas, on
/// whichever axis it runs along.
///
/// The gradient here runs down the *short* axis, which is the direction
/// that discriminates: a shared scale divides y by the width, so on a
/// 96×32 canvas the bottom row would reach only a third of the way along
/// the ramp and the rest of the ramp would never be seen. Along the long
/// axis a shared scale looks correct, so testing that way proves nothing.
#[test]
fn a_gradient_spans_the_short_axis() {
    let json = r##"{
      "name": "rect",
      "nodes": {
        "out": { "op": "gradient-linear", "anchor": "tile",
                 "start": [0, 0], "end": [0, 1],
                 "stops": [[0, "#000000"], [1, "#ffffff"]] }
      },
      "output": "@out"
    }"##;
    let r = render_shaped(json, W, H, 0);
    let top = r.pixel(W / 2, 0)[0] as i32;
    let mid = r.pixel(W / 2, H / 2)[0] as i32;
    let bottom = r.pixel(W / 2, H - 1)[0] as i32;
    assert!(
        top < mid && mid < bottom,
        "gradient should rise down the height: {top} → {mid} → {bottom}"
    );
    assert!(top < 24, "top should be near black, got {top}");
    assert!(bottom > 231, "bottom should be near white, got {bottom}");
    // Halfway down is halfway along the ramp, not a third of the way.
    assert!(
        (mid - 128).abs() < 24,
        "midpoint should be mid-ramp, got {mid}"
    );
}
