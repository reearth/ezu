//! `erode` / `dilate` over a `ScalarField` — shaping an elevation field
//! by min / max before the terrain ops read it.

use crate::common::field_of_node;
use ezu_graph::{build_graph, GeoScale, PortKind, ScalarField, TileId};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

const TILE: TileId = TileId { z: 0, x: 0, y: 0 };
const SIZE: u32 = 24;
const PAD: u32 = 4;
const N: u32 = SIZE + 2 * PAD;

/// A padded-canvas field of `f(x, y)`.
fn field(nodata: Option<f32>, f: impl Fn(u32, u32) -> f32) -> ScalarField {
    let mut values = Vec::with_capacity((N * N) as usize);
    for y in 0..N {
        for x in 0..N {
            values.push(f(x, y));
        }
    }
    ScalarField {
        width: N,
        height: N,
        values: values.into(),
        nodata,
        geo_scale: Some(GeoScale {
            metres_per_pixel_x: 12.5,
            metres_per_pixel_y: 12.5,
        }),
    }
}

/// `dem → <op> → color-ramp`, the morphology node named `shaped`.
fn doc(op: &str, radius: u32) -> String {
    format!(
        r##"{{
          "name": "morphology-field-test",
          "tile-size": {SIZE},
          "pad": {PAD},
          "sources": {{
            "terrain": {{ "type": "dem",
                          "url": "http://example.invalid/{{z}}/{{x}}/{{y}}.webp",
                          "encoding": "terrarium" }}
          }},
          "nodes": {{
            "dem": {{ "op": "dem" }},
            "shaped": {{ "op": "{op}", "input": "@dem", "radius-px": {radius} }},
            "out": {{ "op": "color-ramp", "field": "@shaped",
                      "stops": [ {{ "value": 0, "color": "#000000" }},
                                 {{ "value": 100, "color": "#ffffff" }} ] }}
          }},
          "output": "@out"
        }}"##
    )
}

fn shaped(op: &str, radius: u32, input: ScalarField) -> std::sync::Arc<ScalarField> {
    field_of_node(
        &doc(op, radius),
        SIZE,
        PAD,
        TILE,
        &[("terrain", input)],
        "shaped",
    )
}

/// A square plateau of height 50, `half` px either side of the centre,
/// on flat ground at 0.
fn peak(half: u32) -> ScalarField {
    let c = N / 2;
    field(None, move |x, y| {
        if x.abs_diff(c) <= half && y.abs_diff(c) <= half {
            50.0
        } else {
            0.0
        }
    })
}

/// How many samples are above ground.
fn footprint(f: &ScalarField) -> usize {
    f.values.iter().filter(|&&v| v > 0.0).count()
}

#[test]
fn erode_shrinks_a_peak_and_dilate_grows_it() {
    let before = footprint(&peak(4));
    assert_eq!(before, 81);
    // A 9 × 9 plateau loses two px off each side, or gains two.
    assert_eq!(footprint(&shaped("erode", 2, peak(4))), 25);
    assert_eq!(footprint(&shaped("dilate", 2, peak(4))), 169);
    // The height itself is kept: min and max pick a sample, never blend.
    let grown = shaped("dilate", 2, peak(4));
    assert!(grown.values.iter().all(|&v| v == 0.0 || v == 50.0));
    // A peak narrower than the window is gone.
    assert_eq!(footprint(&shaped("erode", 2, peak(1))), 0);
}

#[test]
fn nodata_is_ignored() {
    let nodata = -9999.0;
    let c = N / 2;
    // A rising ramp with a nodata hole in the middle. Without leaving
    // the hole out, `erode` would spread -9999 all around it.
    let holey = field(Some(nodata), |x, y| {
        if x.abs_diff(c) <= 1 && y.abs_diff(c) <= 1 {
            nodata
        } else {
            x as f32
        }
    });
    let eroded = shaped("erode", 1, holey.clone());
    let dilated = shaped("dilate", 1, holey);
    for y in 0..N {
        for x in 0..N {
            let i = (y * N + x) as usize;
            let lo = x.saturating_sub(1) as f32;
            let hi = (x + 1).min(N - 1) as f32;
            if x == c && y == c {
                // Every sample in reach of the centre is in the hole.
                assert_eq!(eroded.values[i], nodata);
                assert_eq!(dilated.values[i], nodata);
            } else {
                // Some ramp value in reach, never the nodata value.
                for v in [eroded.values[i], dilated.values[i]] {
                    assert!((lo..=hi).contains(&v), "({x}, {y}) {v}");
                }
            }
        }
    }
}

#[test]
fn the_output_is_a_field_with_its_geo_scale() {
    for op in ["erode", "dilate"] {
        let json = doc(op, 2);
        let graph = build_graph(&Document::from_json(&json).unwrap(), &default_registry()).unwrap();
        assert_eq!(
            graph.output_kind(graph.index_of("shaped").unwrap()),
            PortKind::ScalarField
        );
        let out = shaped(op, 2, field(Some(-1.0), |x, _| x as f32));
        assert_eq!((out.width, out.height), (N, N));
        assert_eq!(out.nodata, Some(-1.0));
        let scale = out.geo_scale.expect("geo_scale kept");
        assert_eq!(scale.metres_per_pixel_x, 12.5);
    }
}
