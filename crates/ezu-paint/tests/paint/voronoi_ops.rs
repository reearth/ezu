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
                   "densify-px": 0.8, "min-branch-px": 1.6 },
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

/// A brick bond as Voronoi cells: staggered seeds, stretched cells, and
/// each cell's `random` driving its grey. The tile is 32 px over the
/// default 4096 extent, so seeds every 1024 × 512 units are 8 × 4 px apart.
fn brick_cells_json() -> String {
    r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "area":  { "op": "tile-bounds", "cover": "canvas" },
        "seeds": { "op": "point-grid", "anchor": "world", "spacing": 1024,
                   "spacing-y": 512, "stagger": 0.5 },
        "cells": { "op": "voronoi-fracture", "features": "@area", "seeds": "@seeds",
                   "aspect": 8 },
        "out":   { "op": "fill-solid", "features": "@cells", "fill": "#000000",
                   "fill-expr": ["interpolate", ["linear"], ["get", "random"],
                                 0, "#000000", 1, "#ffffff"] }
      },
      "output": "@out"
    }"##
    .to_string()
}

/// Every cell gets its own `random`, so the cells come out in many tones.
#[test]
fn voronoi_fracture_gives_each_cell_a_random_value() {
    let r = render(&brick_cells_json(), 32, 8);
    let mut greys: Vec<u8> = (8..40)
        .flat_map(|y| (8..40).map(move |x| (x, y)))
        .map(|(x, y)| r.pixel(x, y)[0])
        .collect();
    greys.sort_unstable();
    greys.dedup();
    assert!(greys.len() >= 8, "only {} tones: {greys:?}", greys.len());
}

/// A cell is seeded from its site's world position, so two tiles that
/// both draw a cell along their shared border draw it in the same tone.
#[test]
fn voronoi_fracture_random_agrees_across_tiles() {
    use ezu_graph::TileId;
    let json = brick_cells_json();
    let left = crate::common::render_tile(&json, 32, 8, TileId { z: 1, x: 0, y: 0 });
    let right = crate::common::render_tile(&json, 32, 8, TileId { z: 1, x: 1, y: 0 });
    // The canvas is 8 px of padding, the 32 px tile, then 8 more. Four
    // pixels past the left tile's right edge is four pixels into the right
    // tile.
    for y in [10, 17, 23, 30, 37] {
        assert_eq!(
            left.pixel(8 + 32 + 4, y),
            right.pixel(8 + 4, y),
            "row {y} differs across the border"
        );
    }
}
