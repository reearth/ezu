//! Integration tests for `Features → Features` geometry ops:
//! `bbox`, `transform`, `smooth`, `densify`, `resample`,
//! `feature-boolean`, `triangulate`, `junctions`. All driven through
//! `literal-geometry` to stay hermetic.

use crate::common::render;

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

/// `buffer`'s `distance` is in canvas pixels: a point inflated by 8 px
/// on a 64 px tile is a disk 8 px in radius, not 8 extent units (1/8 px).
#[test]
fn buffer_distance_is_in_pixels() {
    let json = r##"{
      "name": "demo",
      "tile-size": 64,
      "nodes": {
        "bg":   { "op": "solid", "color": "#ffffff" },
        "pt":   { "op": "literal-geometry", "points": [[2048, 2048]] },
        "disk": { "op": "buffer", "features": "@pt", "distance": 8, "join": "round" },
        "fill": { "op": "fill-solid", "features": "@disk", "fill": "#000000" },
        "out":  { "op": "blend", "base": "@bg", "over": "@fill" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 64, 0);
    let inside = r.pixel(32 + 6, 32);
    assert!(
        inside[0] < 40,
        "6 px from the point is inside the disk: {inside:?}"
    );
    let outside = r.pixel(32 + 11, 32);
    assert!(
        outside[0] > 215,
        "11 px from the point is outside it: {outside:?}"
    );
}

/// `hatch`'s `spacing` is in canvas pixels: 16 px over a 64 px tile is
/// four lines, whatever the feature extent.
#[test]
fn hatch_spacing_is_in_pixels() {
    let json = r##"{
      "name": "demo",
      "tile-size": 64,
      "nodes": {
        "bg":    { "op": "solid", "color": "#ffffff" },
        "area":  { "op": "tile-bounds" },
        "lines": { "op": "hatch", "features": "@area", "spacing": 16 },
        "draw":  { "op": "stroke", "features": "@lines", "color": "#000000", "width-px": 2 },
        "out":   { "op": "blend", "base": "@bg", "over": "@draw" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 64, 0);
    // Count dark runs down the middle column: one per line.
    let mut runs = 0;
    let mut dark = false;
    for y in 0..64 {
        let d = r.pixel(32, y)[0] < 128;
        if d && !dark {
            runs += 1;
        }
        dark = d;
    }
    assert!(
        (3..=5).contains(&runs),
        "expected ~4 hatch lines, got {runs}"
    );
}

/// `tile-bounds` is the visible tile by default; `cover: "canvas"` takes in
/// the padding too, so shapes cut from it carry on past the tile's edge.
#[test]
fn tile_bounds_cover_canvas_reaches_into_the_padding() {
    let doc = |cover: &str| {
        format!(
            r##"{{
      "name": "demo",
      "tile-size": 32,
      "nodes": {{
        "area": {{ "op": "tile-bounds", "cover": "{cover}" }},
        "out":  {{ "op": "fill-solid", "features": "@area", "fill": "#000000" }}
      }},
      "output": "@out"
    }}"##
        )
    };
    // The canvas is 32 px plus 8 px of padding a side; (4, 24) is in the
    // left margin, (24, 24) the middle of the tile.
    let tile = render(&doc("tile"), 32, 8);
    assert_eq!(tile.pixel(24, 24)[3], 255, "the tile itself is filled");
    assert_eq!(tile.pixel(4, 24)[3], 0, "`tile` stops at the tile's edge");
    let canvas = render(&doc("canvas"), 32, 8);
    assert_eq!(canvas.pixel(4, 24)[3], 255, "`canvas` fills the margin too");
}

/// `junctions` feeds `stamp` directly: a horizontal bar rotated by each
/// junction's `axis-deg` crosses the line it marks. Two sections meeting
/// in the middle of a horizontal line get a vertical tick there, and the
/// corner of an L gets a diagonal one.
#[test]
fn junction_ticks_cross_the_lines_they_mark() {
    let json = r##"{
      "name": "demo",
      "tile-size": 64,
      "nodes": {
        "bg":    { "op": "solid", "color": "#ffffff" },
        "lines": { "op": "literal-geometry",
                   "lines": [[[512, 1024], [2048, 1024]], [[2048, 1024], [3584, 1024]],
                             [[1024, 2048], [1024, 3584]], [[1024, 3584], [3072, 3584]]] },
        "j":     { "op": "junctions", "features": "@lines" },
        "bar":   { "op": "solid", "kind": "sprite", "color": "#000000",
                   "width-px": 13, "height-px": 3 },
        "ticks": { "op": "stamp", "features": "@j", "image": "@bar",
                   "rotation-deg-expr": ["get", "axis-deg"] },
        "out":   { "op": "blend", "base": "@bg", "over": "@ticks" }
      },
      "output": "@out"
    }"##;
    let r = render(json, 64, 0);
    let dark = |x: u32, y: u32| r.pixel(x, y)[0] < 128;
    // The straight continuation at (32, 16): a vertical tick.
    assert!(dark(32, 11) && dark(32, 21), "vertical tick at (32, 16)");
    assert!(!dark(27, 16) && !dark(37, 16), "not a horizontal one");
    // The L's corner at (16, 56), arms up and right: the tick runs along
    // the bisector, up-right to down-left.
    assert!(dark(20, 52) && dark(12, 60), "diagonal tick at (16, 56)");
    assert!(
        !dark(12, 52) && !dark(20, 60),
        "on the bisector, not across it"
    );
}
