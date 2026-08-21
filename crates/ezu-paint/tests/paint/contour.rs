//! `contour` — isolines from a `ScalarField`, checked end-to-end by
//! painting them with `stroke` (which also proves the tile-local
//! coordinate round-trip: contour converts canvas px → tile units, and
//! stroke scales them back onto the same pixels).

mod common;
use common::render_with_scalar_fields;
use ezu_graph::{ScalarField, TileId};

const TILE: TileId = TileId { z: 0, x: 0, y: 0 };

/// A padded-canvas-sized field with per-sample values from `f(x, y)`.
fn field(size: u32, f: impl Fn(u32, u32) -> f32) -> ScalarField {
    let mut values = Vec::with_capacity((size * size) as usize);
    for y in 0..size {
        for x in 0..size {
            values.push(f(x, y));
        }
    }
    ScalarField {
        width: size,
        height: size,
        values: values.into(),
        nodata: None,
        geo_scale: None,
    }
}

/// A `dem → contour → stroke` doc; `contour_fields` is spliced into the
/// contour node, `stroke_fields` into the stroke node.
fn doc(tile_size: u32, pad: u32, contour_fields: &str, stroke_fields: &str) -> String {
    format!(
        r##"{{
          "name": "contour-test",
          "tile-size": {tile_size},
          "pad": {pad},
          "sources": {{
            "terrain": {{ "type": "dem",
                          "url": "http://example.invalid/{{z}}/{{x}}/{{y}}.webp",
                          "encoding": "terrarium" }}
          }},
          "nodes": {{
            "dem":  {{ "op": "dem" }},
            "iso":  {{ "op": "contour", "field": "@dem"{contour_fields} }},
            "out":  {{ "op": "stroke", "features": "@iso", "color": "#ff0000",
                       "width-px": 2{stroke_fields} }}
          }},
          "output": "@out"
        }}"##
    )
}

fn alpha(r: &ezu_graph::RasterBuf, x: u32, y: u32) -> u8 {
    r.pixel(x, y)[3]
}

#[test]
fn linear_gradient_yields_straight_evenly_spaced_isolines() {
    // Field value = grid x. Interval 8 (min 1 keeps the degenerate
    // level-0 line at the canvas edge out) → vertical lines where the
    // field crosses 8, 16, 24: grid x = 8 → pixel column 8 (the vertex
    // sits at pixel center x + 0.5, so the test stroke covers that
    // column fully).
    let json = doc(32, 0, r##", "interval": 8, "min": 1"##, "");
    let f = field(32, |x, _| x as f32);
    let r = render_with_scalar_fields(&json, 32, 0, TILE, &[("terrain", f)]);

    for col in [8u32, 16, 24] {
        // Straight and full-height.
        for y in [1u32, 16, 30] {
            assert!(
                alpha(&r, col, y) > 200,
                "column {col} at y={y} should be stroked"
            );
        }
    }
    for col in [4u32, 12, 20, 28] {
        assert_eq!(alpha(&r, col, 16), 0, "column {col} lies between levels");
    }
}

#[test]
fn base_offsets_the_level_grid() {
    let json = doc(32, 0, r##", "interval": 8, "base": 4"##, "");
    let f = field(32, |x, _| x as f32);
    let r = render_with_scalar_fields(&json, 32, 0, TILE, &[("terrain", f)]);
    // Levels 4, 12, 20, 28 instead of 8, 16, 24.
    assert!(alpha(&r, 12, 16) > 200, "level 12 line expected");
    assert_eq!(alpha(&r, 8, 16), 0, "no level at 8 with base 4");
}

#[test]
fn min_max_clamp_the_emitted_levels() {
    let json = doc(32, 0, r##", "interval": 8, "min": 1, "max": 20"##, "");
    let f = field(32, |x, _| x as f32);
    let r = render_with_scalar_fields(&json, 32, 0, TILE, &[("terrain", f)]);
    assert!(alpha(&r, 8, 16) > 200);
    assert!(alpha(&r, 16, 16) > 200);
    assert_eq!(alpha(&r, 24, 16), 0, "level 24 is above max 20");
}

#[test]
fn levels_array_overrides_interval() {
    // With `levels: [10]` a radial field yields a single closed ring of
    // radius 10 around the peak — and none of the rings `interval: 3`
    // would have produced.
    let json = doc(32, 0, r##", "interval": 3, "levels": [10]"##, "");
    let f = field(32, |x, y| {
        let (dx, dy) = (x as f32 - 16.0, y as f32 - 16.0);
        (dx * dx + dy * dy).sqrt()
    });
    let r = render_with_scalar_fields(&json, 32, 0, TILE, &[("terrain", f)]);

    // The ring passes through the four compass points 10px from the
    // center (grid (16, 16) → canvas (16.5, 16.5)).
    for (x, y) in [(26u32, 16u32), (6, 16), (16, 26), (16, 6)] {
        assert!(alpha(&r, x, y) > 100, "ring should pass ({x}, {y})");
    }
    // Closed-ish: it also crosses the diagonal (10/√2 ≈ 7.07 off-center).
    let diag = [(23u32, 23u32), (24, 23), (23, 24), (24, 24)]
        .iter()
        .map(|&(x, y)| alpha(&r, x, y) as u32)
        .max()
        .unwrap();
    assert!(diag > 100, "ring should cross the diagonal");
    // No interval-3 rings: radius 6 stays clean.
    assert_eq!(alpha(&r, 22, 16), 0, "interval must be overridden");
    assert_eq!(alpha(&r, 16, 16), 0, "center is not on the ring");
}

#[test]
fn level_property_drives_data_driven_paint() {
    // Each level's group carries `{"level": <number>}`; a stroke
    // `color-expr` matching on it paints level 16 red and the rest blue —
    // proving the property is present and numeric.
    let json = doc(
        32,
        0,
        r##", "interval": 8, "min": 1"##,
        r##", "color-expr": ["case", ["==", ["get", "level"], 16], "#ff0000", "#0000ff"]"##,
    );
    let f = field(32, |x, _| x as f32);
    let r = render_with_scalar_fields(&json, 32, 0, TILE, &[("terrain", f)]);

    let (red, blue) = (r.pixel(16, 16), r.pixel(8, 16));
    assert!(
        red[0] > 200 && red[2] < 50,
        "level 16 should be red: {red:?}"
    );
    assert!(
        blue[2] > 200 && blue[0] < 50,
        "level 8 should be blue: {blue:?}"
    );
}

#[test]
fn tile_local_round_trip_survives_pad() {
    // With pad 4 the field is 40px wide; value = canvas x − pad puts
    // level 8 at canvas column 12. Contour converts that to tile units
    // (negative/overflowing in the pad region included); stroke scales
    // it back — the line must land on column 12 of the padded canvas
    // and run through the pad rows too.
    let (tile_size, pad) = (32u32, 4u32);
    let json = doc(tile_size, pad, r##", "levels": [8]"##, "");
    let f = field(tile_size + 2 * pad, |x, _| x as f32 - pad as f32);
    let r = render_with_scalar_fields(&json, tile_size, pad, TILE, &[("terrain", f)]);

    for y in [1u32, 4, 20, 38] {
        assert!(
            alpha(&r, 12, y) > 200,
            "round-tripped line should cover column 12 at y={y} (pad rows included)"
        );
    }
    for col in [8u32, 16] {
        assert_eq!(alpha(&r, col, 20), 0, "column {col} must stay clean");
    }
}
