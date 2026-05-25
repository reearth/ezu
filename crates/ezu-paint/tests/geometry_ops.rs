//! Integration tests for `Features → Features` geometry ops:
//! `bbox`, `transform`, `smooth`, `densify`, `resample`,
//! `feature-boolean`, `triangulate`. All driven through
//! `literal-geometry` to stay hermetic.

mod common;
use common::render;

/// `bbox` over a sparse point set, filled, should colour the
/// rectangle covering the points.
#[test]
fn bbox_fills_rectangle_covering_points() {
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "bg":   { "op": "solid", "color": "#ffffff" },
        "pts":  { "op": "literal-geometry",
                  "points": [[500, 500], [3500, 500], [3500, 3500], [500, 3500]] },
        "box":  { "op": "bbox", "features": "@pts" },
        "fill": { "op": "fill-solid", "features": "@box", "fill": "#33cc33" },
        "out":  { "op": "blend", "base": "@bg", "over": "@fill" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Centre of the bbox should be green-tinted; corner outside the
    // bbox stays white.
    let centre = r.pixel(16, 16);
    assert!(
        centre[1] > centre[0] + 40 && centre[1] > centre[2] + 40,
        "bbox centre should be green-dominant: {centre:?}"
    );
    let outside = r.pixel(0, 0);
    assert!(
        outside[0] > 240 && outside[1] > 240 && outside[2] > 240,
        "outside bbox should stay white: {outside:?}"
    );
}

/// `transform` with a 90° rotation should swap visible axis position
/// for a non-symmetric polygon.
#[test]
fn transform_rotates_polygon_visibly() {
    // Horizontal bar polygon along the top, then rotated 90° around
    // the tile centre.
    let json_plain = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "bg":   { "op": "solid", "color": "#ffffff" },
        "bar":  { "op": "literal-geometry",
                  "polygons": [{ "exterior": [[200, 1800], [3800, 1800], [3800, 2200], [200, 2200]] }] },
        "fill": { "op": "fill-solid", "features": "@bar", "fill": "#222222" },
        "out":  { "op": "blend", "base": "@bg", "over": "@fill" }
      },
      "output": "@out"
    }"##;
    let json_rotated = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "bg":    { "op": "solid", "color": "#ffffff" },
        "bar":   { "op": "literal-geometry",
                   "polygons": [{ "exterior": [[200, 1800], [3800, 1800], [3800, 2200], [200, 2200]] }] },
        "rot":   { "op": "transform", "features": "@bar",
                   "rotation-deg": 90, "pivot": [2048, 2048] },
        "fill":  { "op": "fill-solid", "features": "@rot", "fill": "#222222" },
        "out":   { "op": "blend", "base": "@bg", "over": "@fill" }
      },
      "output": "@out"
    }"##;
    let plain = render(json_plain, 32, 0);
    let rotated = render(json_rotated, 32, 0);
    // Plain: horizontal bar — y=16 is dark, x=16 alone is not enough info.
    // Sample a pixel that's dark in plain but light in rotated (and vice versa).
    let plain_horiz = plain.pixel(8, 16);
    let rotated_horiz = rotated.pixel(8, 16);
    let plain_vert = plain.pixel(16, 8);
    let rotated_vert = rotated.pixel(16, 8);
    // Plain has horizontal bar through (8, 16); rotated puts vertical bar through (16, 8).
    assert!(
        plain_horiz[0] < 80,
        "plain horiz sample should be dark: {plain_horiz:?}"
    );
    assert!(
        rotated_vert[0] < 80,
        "rotated vert sample should be dark: {rotated_vert:?}"
    );
    // The OFF-axis samples should be light in their respective renders.
    assert!(
        rotated_horiz[0] > 200 || plain_vert[0] > 200,
        "rotation should swap which axis is dark"
    );
}

/// `feature-boolean` difference: subtract one square from another and
/// confirm the difference region is filled where expected.
#[test]
fn feature_boolean_difference_punches_a_hole() {
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "bg":   { "op": "solid", "color": "#ffffff" },
        "outer":{ "op": "literal-geometry",
                  "polygons": [{ "exterior": [[200, 200], [3800, 200], [3800, 3800], [200, 3800]] }] },
        "inner":{ "op": "literal-geometry",
                  "polygons": [{ "exterior": [[1500, 1500], [2500, 1500], [2500, 2500], [1500, 2500]] }] },
        "ring": { "op": "feature-boolean", "a": "@outer", "b": "@inner", "mode": "difference" },
        "fill": { "op": "fill-solid", "features": "@ring", "fill": "#cc3333" },
        "out":  { "op": "blend", "base": "@bg", "over": "@fill" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    // Outer corner area (4, 4) should be red; inner hole area (16, 16) should be white.
    let outer = r.pixel(4, 4);
    let inner = r.pixel(16, 16);
    assert!(
        outer[0] > outer[1] + 40,
        "outer ring should be red-dominant: {outer:?}"
    );
    assert!(
        inner[0] > 240 && inner[1] > 240 && inner[2] > 240,
        "inner hole should stay white: {inner:?}"
    );
}

/// `smooth` of a sharp diamond should still produce drawable polygons
/// (not collapse to nothing).
#[test]
fn smooth_produces_drawable_polygon() {
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "bg":     { "op": "solid", "color": "#ffffff" },
        "diam":   { "op": "literal-geometry",
                    "polygons": [{ "exterior": [[2048, 500], [3500, 2048], [2048, 3500], [500, 2048]] }] },
        "smooth": { "op": "smooth", "features": "@diam", "iterations": 3 },
        "fill":   { "op": "fill-solid", "features": "@smooth", "fill": "#3366cc" },
        "out":    { "op": "blend", "base": "@bg", "over": "@fill" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    let centre = r.pixel(16, 16);
    assert!(
        centre[2] > centre[0] + 40,
        "smoothed diamond centre should be blue-dominant: {centre:?}"
    );
}

/// `triangulate` on 4 corners → 2 triangles, both rendered.
#[test]
fn triangulate_fills_the_convex_hull() {
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "bg":   { "op": "solid", "color": "#ffffff" },
        "pts":  { "op": "literal-geometry",
                  "points": [[500, 500], [3500, 500], [3500, 3500], [500, 3500]] },
        "tri":  { "op": "triangulate", "features": "@pts" },
        "fill": { "op": "fill-solid", "features": "@tri", "fill": "#229922" },
        "out":  { "op": "blend", "base": "@bg", "over": "@fill" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 32, 0);
    let centre = r.pixel(16, 16);
    assert!(
        centre[1] > centre[0] + 40,
        "centre of triangulated quad should be green-dominant: {centre:?}"
    );
}
