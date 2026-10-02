//! `flow-field` — direction rasters from an elevation field, checked on
//! planar ramps where the answer is known exactly.

use crate::common::render_with_scalar_fields;
use ezu_graph::{build_graph, ScalarField, TileId};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

const TILE: TileId = TileId { z: 0, x: 0, y: 0 };
const SIZE: u32 = 16;

/// A field with no geographic scale, so the gradient is in elevation
/// units per pixel: `z = x` is a 45° slope.
fn field(f: impl Fn(u32, u32) -> f32) -> ScalarField {
    let mut values = Vec::with_capacity((SIZE * SIZE) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            values.push(f(x, y));
        }
    }
    ScalarField {
        width: SIZE,
        height: SIZE,
        values: values.into(),
        nodata: None,
        geo_scale: None,
    }
}

/// A `dem → flow-field` doc; `extra` is spliced into the flow-field node.
fn doc(extra: &str) -> String {
    format!(
        r##"{{
          "name": "flow-field-test",
          "tile-size": {SIZE},
          "sources": {{
            "terrain": {{ "type": "dem",
                          "url": "http://example.invalid/{{z}}/{{x}}/{{y}}.webp",
                          "encoding": "terrarium" }}
          }},
          "nodes": {{
            "dem": {{ "op": "dem" }},
            "out": {{ "op": "flow-field", "field": "@dem"{extra} }}
          }},
          "output": "@out"
        }}"##
    )
}

/// The flow-field pixel at the centre of the tile, away from the edge
/// where the 3×3 window is clamped.
fn centre(extra: &str, f: impl Fn(u32, u32) -> f32) -> [u8; 4] {
    let r = render_with_scalar_fields(&doc(extra), SIZE, 0, TILE, &[("terrain", field(f))]);
    r.pixel(SIZE / 2, SIZE / 2)
}

#[test]
fn downhill_points_down_a_ramp() {
    // Rising to the east at 45°: downhill is due west, full length.
    assert_eq!(centre("", |x, _| x as f32), [0, 128, 0, 255]);
}

#[test]
fn uphill_is_the_opposite_of_downhill() {
    assert_eq!(
        centre(r#", "direction": "uphill""#, |x, _| x as f32),
        [255, 128, 0, 255]
    );
}

#[test]
fn along_runs_on_the_contour_with_downhill_on_the_right() {
    // Downhill is west (-x). Facing south puts west on your right (and
    // north-facing would put it on the left), so the contour vector must
    // point south (+y).
    assert_eq!(
        centre(r#", "direction": "along""#, |x, _| x as f32),
        [128, 255, 0, 255]
    );
    // And on a slope falling to the south, the contour runs east.
    assert_eq!(
        centre(r#", "direction": "along""#, |_, y| -(y as f32)),
        [255, 128, 0, 255]
    );
}

#[test]
fn north_facing_slope_points_up_the_raster() {
    // Elevation rising southwards, so the ground falls to the north:
    // downhill is -y, which is G below the 0.5 midpoint.
    assert_eq!(centre("", |_, y| y as f32), [128, 0, 0, 255]);
    assert_eq!(
        centre(r#", "direction": "uphill""#, |_, y| y as f32),
        [128, 255, 0, 255]
    );
}

#[test]
fn flat_ground_is_mid_grey() {
    assert_eq!(centre("", |_, _| 100.0), [128, 128, 0, 255]);
    assert_eq!(
        centre(r#", "normalize": true"#, |_, _| 100.0),
        [128, 128, 0, 255]
    );
}

#[test]
fn length_scales_with_slope_over_max_deg() {
    // 45° against a 90° ceiling: half length, 0.5 - 0.25 = 0.25 → 64.
    assert_eq!(
        centre(r#", "max-deg": 90"#, |x, _| x as f32),
        [64, 128, 0, 255]
    );
    // Steeper than the default 45° ceiling: capped at full length.
    assert_eq!(centre("", |x, _| 3.0 * x as f32), [0, 128, 0, 255]);
}

#[test]
fn normalize_gives_gentle_slopes_full_length() {
    // About 5.7°: an eighth of the default ceiling, but full length
    // once normalised.
    let gentle = |x: u32, _| 0.1 * x as f32;
    let scaled = centre("", gentle);
    assert!(scaled[0] > 100 && scaled[0] < 128, "{scaled:?}");
    assert_eq!(centre(r#", "normalize": true"#, gentle), [0, 128, 0, 255]);
}

#[test]
fn exaggeration_steepens_the_slope() {
    // 45° doubled is atan 2 ≈ 63.4°, still under a 90° ceiling, so the
    // vector grows from half length to about 0.70.
    let r = centre(r#", "max-deg": 90, "exaggeration": 2"#, |x, _| x as f32);
    assert_eq!(r, [38, 128, 0, 255]);
}

#[test]
fn unknown_direction_is_rejected() {
    let doc = Document::from_json(&doc(r#", "direction": "sideways""#)).expect("parse");
    assert!(
        build_graph(&doc, &default_registry()).is_err(),
        "an unknown direction must fail at build"
    );
}
