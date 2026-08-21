//! Smoke tests for the Voronoi-family geometry ops:
//! `voronoi` (point set → edge polylines), `voronoi-fracture`
//! (polygon → sub-polygons via seed points), `medial-axis`
//! (polygon → skeleton polylines).
//!
//! All three are driven through `literal-geometry` so the tests are
//! hermetic — no asset bindings, no network.

use crate::common::render;

/// A 32-canvas with: three-seed point set → `voronoi` → `line`.
/// Should render at least one visible line pixel.
#[test]
fn voronoi_emits_drawable_polylines() {
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "bg":    { "op": "solid", "color": "#ffffff" },
        "seeds": { "op": "literal-geometry",
                   "points": [[200, 200], [3800, 200], [2000, 3800]] },
        "edges": { "op": "voronoi", "features": "@seeds" },
        "draw":  { "op": "line", "features": "@edges",
                   "brush": "@b", "color": "#000000",
                   "radius-px": 1.5, "opacity": 1.0 },
        "out":   { "op": "blend", "base": "@bg", "over": "@draw" },
        "b":     { "op": "brush-solid", "width-px": 1.5, "color": "#000000" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Somewhere on the canvas there should be a dark line pixel.
    let mut any_dark = false;
    for y in 0..32 {
        for x in 0..32 {
            let p = r.pixel(x, y);
            if (p[0] as u32 + p[1] as u32 + p[2] as u32) < 300 {
                any_dark = true;
                break;
            }
        }
        if any_dark {
            break;
        }
    }
    assert!(any_dark, "no voronoi edge pixels rendered");
}

#[test]
fn voronoi_fracture_returns_pieces_inside_polygon() {
    // Square polygon with three seeds → three Voronoi sub-pieces.
    // Render `fill-solid` over the fractured polygons and confirm
    // pixels inside the original polygon are filled.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "bg":    { "op": "solid", "color": "#ffffff" },
        "shape": { "op": "literal-geometry",
                   "polygons": [{ "exterior": [[500, 500], [3500, 500], [3500, 3500], [500, 3500]] }] },
        "seeds": { "op": "literal-geometry",
                   "points": [[1000, 2000], [3000, 2000], [2000, 3000]] },
        "frag":  { "op": "voronoi-fracture",
                   "features": "@shape", "seeds": "@seeds" },
        "fill":  { "op": "fill-solid", "features": "@frag", "fill": "#cc3333" },
        "out":   { "op": "blend", "base": "@bg", "over": "@fill" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Centre of the polygon should be filled in a reddish tint
    // (fill-solid blends `fill` with the existing canvas; we don't
    // care about the exact shade, only that R dominates G/B).
    let centre = r.pixel(16, 16);
    assert!(
        centre[0] > 150 && centre[0] > centre[1] + 40 && centre[0] > centre[2] + 40,
        "fractured polygon centre should be red-dominant: {centre:?}"
    );
    // Outside the polygon (top-left corner) should still be white.
    let corner = r.pixel(1, 1);
    assert!(
        corner[0] > 240 && corner[1] > 240 && corner[2] > 240,
        "corner should remain white: {corner:?}"
    );
}

#[test]
fn medial_axis_of_long_rectangle_renders_a_line() {
    // 32-canvas tile, extent 4096. A long horizontal rectangle in
    // tile-extent coords; medial axis is a horizontal line down its
    // centre. Render with `line` and check there's at least one dark
    // pixel near the rectangle's vertical centre.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "bg":    { "op": "solid", "color": "#ffffff" },
        "rect":  { "op": "literal-geometry",
                   "polygons": [{ "exterior": [[200, 1800], [3800, 1800], [3800, 2200], [200, 2200]] }] },
        "axis":  { "op": "medial-axis", "features": "@rect",
                   "densify-px": 100, "min-branch-px": 200 },
        "draw":  { "op": "line", "features": "@axis",
                   "brush": "@b", "color": "#000000",
                   "radius-px": 2.0, "opacity": 1.0 },
        "out":   { "op": "blend", "base": "@bg", "over": "@draw" },
        "b":     { "op": "brush-solid", "width-px": 1.5, "color": "#000000" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Sample along the centre horizontal scanline (y ≈ 16).
    let mut dark_near_centre = false;
    for x in 4..28 {
        let p = r.pixel(x, 16);
        if (p[0] as u32) < 80 {
            dark_near_centre = true;
            break;
        }
    }
    assert!(
        dark_near_centre,
        "medial axis should produce at least one dark pixel on the rectangle's centre line"
    );
}
