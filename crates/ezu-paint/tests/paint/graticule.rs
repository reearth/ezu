//! `graticule`: lines where the projection puts them, and properties
//! that survive the trip to the op that draws them.

use crate::common::{render, render_tile};
use ezu_graph::TileId;

const SIZE: u32 = 256;

/// Style drawing the graticule as black lines, with `extra` spliced into
/// the graticule node's fields.
fn style(extra: &str) -> String {
    format!(
        r##"{{
      "name": "grid",
      "tile-size": {SIZE},
      "nodes": {{
        "bg":   {{ "op": "solid", "color": "#ffffff" }},
        "grid": {{ "op": "graticule"{extra} }},
        "draw": {{ "op": "stroke", "features": "@grid", "width-px": 1.5, "color": "#000000" }},
        "out":  {{ "op": "blend", "base": "@bg", "over": "@draw" }}
      }},
      "output": "@out"
    }}"##
    )
}

fn is_dark(px: [u8; 4]) -> bool {
    px[0] < 128 && px[1] < 128 && px[2] < 128
}

/// Does a full pixel row read as a drawn line?
fn row_is_line(r: &ezu_graph::RasterBuf, y: u32) -> bool {
    (0..SIZE).filter(|&x| is_dark(r.pixel(x, y))).count() > (SIZE as usize * 3 / 4)
}

fn col_is_line(r: &ezu_graph::RasterBuf, x: u32) -> bool {
    (0..SIZE).filter(|&y| is_dark(r.pixel(x, y))).count() > (SIZE as usize * 3 / 4)
}

/// The y row a latitude should land on, at z=0.
fn expected_row(lat: f64) -> u32 {
    (ezu_core::coord::lat_to_world_y(lat) * SIZE as f64).round() as u32
}

#[test]
fn parallels_and_meridians_are_straight_and_full_width() {
    let r = render(&style(""), SIZE, 0);
    // z=0 with the default ladder is a 30° graticule: the equator is a
    // full row, the prime meridian a full column.
    assert!(row_is_line(&r, expected_row(0.0)), "equator missing");
    assert!(col_is_line(&r, SIZE / 2), "prime meridian missing");
    // Between them, nothing.
    let midway = (expected_row(0.0) + expected_row(30.0)) / 2;
    assert!(!row_is_line(&r, midway), "unexpected line at row {midway}");
}

/// The test that distinguishes a graticule from a grid: on a Mercator
/// map, equal steps in latitude are *not* equal steps in y. 60°N sits
/// well north of the third of the map a linear layout would put it at.
#[test]
fn parallel_spacing_follows_the_projection() {
    let r = render(&style(""), SIZE, 0);
    let mercator = expected_row(60.0);
    let linear = ((90.0 - 60.0) / 180.0 * SIZE as f64).round() as u32;
    assert!(
        mercator.abs_diff(linear) > 4,
        "the test cannot tell the two layouts apart"
    );
    assert!(
        row_is_line(&r, mercator),
        "60°N should sit at row {mercator}"
    );
    assert!(
        !row_is_line(&r, linear),
        "60°N should not sit at row {linear}, where a linear grid would put it"
    );
}

#[test]
fn axes_field_selects_one_family() {
    let parallels = render(&style(r#", "axes": "parallels""#), SIZE, 0);
    assert!(
        row_is_line(&parallels, expected_row(0.0)),
        "equator missing"
    );
    assert!(
        !col_is_line(&parallels, SIZE / 2),
        "meridians should be absent"
    );

    let meridians = render(&style(r#", "axes": "meridians""#), SIZE, 0);
    assert!(col_is_line(&meridians, SIZE / 2), "prime meridian missing");
    assert!(
        !row_is_line(&meridians, expected_row(0.0)),
        "parallels should be absent"
    );
}

#[test]
fn explicit_interval_overrides_the_zoom_ladder() {
    // At z=0 the ladder gives 30°, so a 60° line exists by default but a
    // 45° one only appears when the interval is stated.
    let laddered = render(&style(""), SIZE, 0);
    assert!(!row_is_line(&laddered, expected_row(45.0)));

    let explicit = render(&style(r#", "interval-deg": 45"#), SIZE, 0);
    assert!(
        row_is_line(&explicit, expected_row(45.0)),
        "45°N missing with an explicit interval"
    );
    assert!(
        !row_is_line(&explicit, expected_row(30.0)),
        "30°N should be gone with a 45° interval"
    );
}

/// Each line carries `axis`, `degrees` and `label`, which is what lets a
/// downstream op draw or label the two families differently. Colouring
/// by `axis` proves the properties arrive.
#[test]
fn axis_property_reaches_the_drawing_op() {
    let json = format!(
        r##"{{
      "name": "grid",
      "tile-size": {SIZE},
      "nodes": {{
        "bg":   {{ "op": "solid", "color": "#ffffff" }},
        "grid": {{ "op": "graticule" }},
        "draw": {{ "op": "stroke", "features": "@grid", "width-px": 1.5, "color": "#000000",
                  "color-expr": ["match", ["get", "axis"],
                                 "parallel", "#ff0000", "meridian", "#0000ff", "#000000"] }},
        "out":  {{ "op": "blend", "base": "@bg", "over": "@draw" }}
      }},
      "output": "@out"
    }}"##
    );
    let r = render(&json, SIZE, 0);
    // Sample the equator away from any meridian crossing, and the prime
    // meridian away from any parallel.
    let on_parallel = r.pixel(SIZE / 2 + 10, expected_row(0.0));
    assert!(
        on_parallel[0] > 150 && on_parallel[2] < 100,
        "parallel should be red: {on_parallel:?}"
    );
    let on_meridian = r.pixel(SIZE / 2, expected_row(0.0) + 12);
    assert!(
        on_meridian[2] > 150 && on_meridian[0] < 100,
        "meridian should be blue: {on_meridian:?}"
    );
}

/// A meridian is shared by the two tiles either side of it: it lands on
/// one tile's right edge and the next tile's left edge, which is what
/// keeps the grid unbroken across the seam.
#[test]
fn a_shared_meridian_lands_on_both_tiles() {
    let s = style(r#", "interval-deg": 90"#);
    let west = render_tile(&s, SIZE, 0, TileId { z: 1, x: 0, y: 0 });
    let east = render_tile(&s, SIZE, 0, TileId { z: 1, x: 1, y: 0 });
    // The 0° meridian is the boundary between the two tiles.
    assert!(
        col_is_line(&west, SIZE - 1),
        "0° missing from the western tile's edge"
    );
    assert!(
        col_is_line(&east, 0),
        "0° missing from the eastern tile's edge"
    );
    // Each tile also holds the meridian in its own middle: 90°W and 90°E.
    assert!(col_is_line(&west, SIZE / 2), "90°W missing");
    assert!(col_is_line(&east, SIZE / 2), "90°E missing");
}
