//! `sharpen` over a `ScalarField` — steepening an elevation field's
//! ridges before the terrain ops read it.

use crate::common::field_of_node;
use ezu_graph::{build_graph, GeoScale, PortKind, ScalarField, TileId};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

const TILE: TileId = TileId { z: 0, x: 0, y: 0 };
const SIZE: u32 = 24;
const PAD: u32 = 2;
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
            metres_per_pixel_x: 20.0,
            metres_per_pixel_y: 20.0,
        }),
    }
}

/// `dem → sharpen → color-ramp`, the sharpen node named `crisp`.
fn doc(amount: f64) -> String {
    format!(
        r##"{{
          "name": "sharpen-field-test",
          "tile-size": {SIZE},
          "pad": {PAD},
          "sources": {{
            "terrain": {{ "type": "dem",
                          "url": "http://example.invalid/{{z}}/{{x}}/{{y}}.webp",
                          "encoding": "terrarium" }}
          }},
          "nodes": {{
            "dem": {{ "op": "dem" }},
            "crisp": {{ "op": "sharpen", "input": "@dem", "amount": {amount} }},
            "out": {{ "op": "color-ramp", "field": "@crisp",
                      "stops": [ {{ "value": 0, "color": "#000000" }},
                                 {{ "value": 100, "color": "#ffffff" }} ] }}
          }},
          "output": "@out"
        }}"##
    )
}

fn crisp(amount: f64, input: ScalarField) -> std::sync::Arc<ScalarField> {
    field_of_node(
        &doc(amount),
        SIZE,
        PAD,
        TILE,
        &[("terrain", input)],
        "crisp",
    )
}

#[test]
fn a_constant_field_is_unchanged() {
    let json = doc(1.0);
    let graph = build_graph(&Document::from_json(&json).unwrap(), &default_registry()).unwrap();
    assert_eq!(
        graph.output_kind(graph.index_of("crisp").unwrap()),
        PortKind::ScalarField
    );
    let out = crisp(1.0, field(Some(-1.0), |_, _| 432.0));
    assert!(out.values.iter().all(|&v| v == 432.0));
    assert_eq!(out.nodata, Some(-1.0));
    let scale = out.geo_scale.expect("geo_scale kept");
    assert_eq!(scale.metres_per_pixel_y, 20.0);
}

#[test]
fn a_ridge_gets_steeper_flanks() {
    // A tent-shaped ridge along x = c: rising 4 per px, then falling.
    let c = N / 2;
    let ridge = |x: u32, _: u32| 100.0 - 4.0 * x.abs_diff(c) as f32;
    let before = field(None, ridge);
    let after = crisp(0.8, field(None, ridge));
    let at = |f: &[f32], x: u32| f[((N / 2) * N + x) as usize];
    // The crest rises and the foot of each flank sinks, so the drop from
    // the crest to one px down the flank is steeper than it was.
    let drop_before = at(&before.values, c) - at(&before.values, c + 1);
    let drop_after = at(&after.values, c) - at(&after.values, c + 1);
    assert!(at(&after.values, c) > at(&before.values, c));
    assert!(drop_after > drop_before, "{drop_after} vs {drop_before}");
    // On the straight part of the flank the Laplacian is zero, give or
    // take rounding.
    assert!((at(&after.values, c + 4) - at(&before.values, c + 4)).abs() < 1e-3);
}

#[test]
fn nodata_stays_and_does_not_distort_its_rim() {
    let nodata = -9999.0;
    let c = N / 2;
    let holey = field(
        Some(nodata),
        |x, y| {
            if x == c && y == c {
                nodata
            } else {
                50.0
            }
        },
    );
    let out = crisp(1.0, holey);
    for y in 0..N {
        for x in 0..N {
            let v = out.values[(y * N + x) as usize];
            let want = if x == c && y == c { nodata } else { 50.0 };
            assert_eq!(v, want, "({x}, {y})");
        }
    }
}
