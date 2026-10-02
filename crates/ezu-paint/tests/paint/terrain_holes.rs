//! `hillshade`, `slope` and `flow-field` over a DEM with a hole in it:
//! the hole draws nothing (or no flow), and the ground around it shades
//! as the level plane it sits in instead of drawing a cliff down to the
//! nodata value.

use crate::common::render_with_scalar_fields;
use ezu_graph::{GeoScale, RasterBuf, ScalarField, TileId};

const TILE: TileId = TileId { z: 0, x: 0, y: 0 };
const SIZE: u32 = 16;
const LEVEL: f32 = 500.0;

/// The 2×2 hole in the middle of the tile.
fn in_hole(x: u32, y: u32) -> bool {
    (7..9).contains(&x) && (7..9).contains(&y)
}

/// A level plane at [`LEVEL`], with the hole written as `missing` when
/// it is given.
fn plane(nodata: Option<f32>, missing: Option<f32>) -> ScalarField {
    let mut values = Vec::with_capacity((SIZE * SIZE) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            values.push(match missing {
                Some(m) if in_hole(x, y) => m,
                _ => LEVEL,
            });
        }
    }
    ScalarField {
        width: SIZE,
        height: SIZE,
        values: values.into(),
        nodata,
        geo_scale: Some(GeoScale {
            metres_per_pixel_x: 30.0,
            metres_per_pixel_y: 30.0,
        }),
    }
}

/// A `dem → <op>` doc; `node` is the output node's fields.
fn doc(node: &str) -> String {
    format!(
        r##"{{
          "name": "terrain-holes-test",
          "tile-size": {SIZE},
          "sources": {{
            "terrain": {{ "type": "dem",
                          "url": "http://example.invalid/{{z}}/{{x}}/{{y}}.webp",
                          "encoding": "terrarium" }}
          }},
          "nodes": {{
            "dem": {{ "op": "dem" }},
            "out": {{ "field": "@dem", {node} }}
          }},
          "output": "@out"
        }}"##
    )
}

fn render(node: &str, field: ScalarField) -> std::sync::Arc<RasterBuf> {
    render_with_scalar_fields(&doc(node), SIZE, 0, TILE, &[("terrain", field)])
}

/// Render `node` over the plane with a nodata hole and with a NaN hole,
/// and check each against the unbroken plane: the hole is `hole_pixel`
/// and every other pixel, the rim included, is what the plane gives.
fn check_holes(node: &str, hole_pixel: [u8; 4]) {
    let level = render(node, plane(None, None));
    let nodata = -9999.0;
    let holed = [
        ("nodata", render(node, plane(Some(nodata), Some(nodata)))),
        ("NaN", render(node, plane(None, Some(f32::NAN)))),
        (
            "NaN with nodata",
            render(node, plane(Some(nodata), Some(f32::NAN))),
        ),
    ];
    for (kind, r) in holed {
        for y in 0..SIZE {
            for x in 0..SIZE {
                let want = if in_hole(x, y) {
                    hole_pixel
                } else {
                    level.pixel(x, y)
                };
                assert_eq!(r.pixel(x, y), want, "{node}, {kind} hole, ({x}, {y})");
            }
        }
    }
}

const TRANSPARENT: [u8; 4] = [0, 0, 0, 0];
const NO_FLOW: [u8; 4] = [128, 128, 0, 255];

#[test]
fn hillshade_leaves_a_hole_transparent_without_a_rim() {
    check_holes(r#""op": "hillshade""#, TRANSPARENT);
    check_holes(
        r#""op": "hillshade", "multidirectional": true"#,
        TRANSPARENT,
    );
    check_holes(
        r##""op": "hillshade", "mode": "relief", "highlight-color": "#ffffff""##,
        TRANSPARENT,
    );
}

#[test]
fn slope_leaves_a_hole_transparent_without_a_rim() {
    check_holes(r#""op": "slope""#, TRANSPARENT);
    check_holes(r#""op": "slope", "invert": true"#, TRANSPARENT);
}

#[test]
fn flow_field_gives_a_hole_no_flow_and_no_flow_into_it() {
    check_holes(r#""op": "flow-field""#, NO_FLOW);
    check_holes(r#""op": "flow-field", "normalize": true"#, NO_FLOW);
    check_holes(r#""op": "flow-field", "direction": "along""#, NO_FLOW);
}
