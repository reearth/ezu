//! `warp` and `displace` over a `ScalarField` — bending an elevation or
//! noise field before the ops that read it.

use crate::common::{field_of_node, render_with_scalar_fields};
use ezu_graph::{build_graph, GeoScale, PortKind, ScalarField, TileId};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

const TILE: TileId = TileId { z: 0, x: 0, y: 0 };
const SIZE: u32 = 32;
/// Covers the 4 px amplitudes used here, plus the bilinear read's one
/// pixel further.
const PAD: u32 = 6;
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
            metres_per_pixel_x: 30.0,
            metres_per_pixel_y: 30.0,
        }),
    }
}

/// A doc over the bound `terrain` field: `extra_nodes` is spliced in,
/// `dem` is the field and `out` must be a raster.
fn doc(extra_nodes: &str) -> String {
    format!(
        r##"{{
          "name": "warp-field-test",
          "tile-size": {SIZE},
          "pad": {PAD},
          "sources": {{
            "terrain": {{ "type": "dem",
                          "url": "http://example.invalid/{{z}}/{{x}}/{{y}}.webp",
                          "encoding": "terrarium" }}
          }},
          "nodes": {{
            "dem": {{ "op": "dem" }},
            {extra_nodes}
          }},
          "output": "@out"
        }}"##
    )
}

/// `dem → warp → color-ramp`, the warp named `bent`.
fn warp_doc() -> String {
    doc(
        r##""bent": { "op": "warp", "input": "@dem", "type": "perlin",
                      "scale-px": 9, "amp-px": 4, "seed": 3 },
            "out": { "op": "color-ramp", "field": "@bent",
                     "stops": [ { "value": 0, "color": "#000000" },
                                { "value": 200, "color": "#ffffff" } ] }"##,
    )
}

#[test]
fn a_warped_constant_field_stays_constant() {
    let json = warp_doc();
    let graph = build_graph(&Document::from_json(&json).unwrap(), &default_registry()).unwrap();
    assert_eq!(
        graph.output_kind(graph.index_of("bent").unwrap()),
        PortKind::ScalarField
    );

    let flat = field(Some(-9999.0), |_, _| 123.25);
    let out = field_of_node(&json, SIZE, PAD, TILE, &[("terrain", flat)], "bent");
    assert_eq!((out.width, out.height), (N, N));
    assert!(out.values.iter().all(|&v| v == 123.25));
    assert_eq!(out.nodata, Some(-9999.0));
    let scale = out.geo_scale.expect("geo_scale kept");
    assert_eq!(scale.metres_per_pixel_x, 30.0);
}

#[test]
fn a_uniform_displacement_shifts_the_field() {
    // R = 255 is a full `amp-x-px` to the right, and `amp-y-px: 0` keeps
    // the rows: every pixel reads the field 3 px to its right.
    let json = doc(r##""push": { "op": "solid", "color": "#ff0000" },
            "moved": { "op": "displace", "input": "@dem", "displacement": "@push",
                       "amp-x-px": 3, "amp-y-px": 0, "amp-px": 3 },
            "out": { "op": "color-ramp", "field": "@moved",
                     "stops": [ { "value": 0, "color": "#000000" },
                                { "value": 1, "color": "#ffffff" } ] }"##);
    let f = |x: u32, y: u32| (x * x) as f32 * 0.5 + y as f32 * 3.0;
    let out = field_of_node(
        &json,
        SIZE,
        PAD,
        TILE,
        &[("terrain", field(None, f))],
        "moved",
    );
    for y in 0..N {
        for x in 0..N - 3 {
            assert_eq!(out.values[(y * N + x) as usize], f(x + 3, y), "({x}, {y})");
        }
    }
}

#[test]
fn nodata_does_not_bleed_into_its_neighbours() {
    let nodata = -9999.0;
    let holey = |nodata_value: f32| {
        field(Some(nodata_value).filter(|v| !v.is_nan()), move |x, y| {
            if (18..28).contains(&x) && (18..28).contains(&y) {
                nodata_value
            } else {
                100.0
            }
        })
    };
    let json = warp_doc();
    let out = field_of_node(
        &json,
        SIZE,
        PAD,
        TILE,
        &[("terrain", holey(nodata))],
        "bent",
    );
    // Every read is either all-nodata, and stays nodata, or a blend of
    // the 100s around the hole; nothing in between.
    let mut holes = 0;
    for &v in out.values.iter() {
        if v == nodata {
            holes += 1;
        } else {
            assert!((v - 100.0).abs() < 1e-3, "{v}");
        }
    }
    assert!(holes > 0, "the middle of the hole stays nodata");

    // Without a nodata value, NaN is the missing marker and the fill.
    let out = field_of_node(
        &json,
        SIZE,
        PAD,
        TILE,
        &[("terrain", holey(f32::NAN))],
        "bent",
    );
    assert_eq!(out.nodata, None);
    assert!(out.values.iter().any(|v| v.is_nan()));
    assert!(out
        .values
        .iter()
        .all(|&v| v.is_nan() || (v - 100.0).abs() < 1e-3));
}

#[test]
fn a_world_anchored_warp_of_noise_is_seamless_across_adjacent_tiles() {
    // World-anchored scalar noise warped by a world-anchored warp: where
    // two padded tiles overlap, every value whose read stays clear of
    // both canvases' clamped edges must be bit-identical.
    let json = r##"{
      "name": "demo",
      "tile-size": 32,
      "nodes": {
        "src":  { "op": "noise", "kind": "scalar", "type": "perlin",
                  "scale-px": 24, "seed": 11, "anchor": "world" },
        "bent": { "op": "warp", "input": "@src", "type": "perlin",
                  "scale-px": 16, "amp-px": 4, "seed": 23, "anchor": "world" },
        "out":  { "op": "color-ramp", "field": "@bent",
                  "stops": [ { "value": -1, "color": "#000000" },
                             { "value": 1, "color": "#ffffff" } ] }
      },
      "output": "@out"
    }"##;
    let (tile, pad, reach) = (32u32, 8u32, 5u32);
    let n = tile + 2 * pad;
    let left = field_of_node(json, tile, pad, TileId { z: 4, x: 5, y: 7 }, &[], "bent");
    let right = field_of_node(json, tile, pad, TileId { z: 4, x: 6, y: 7 }, &[], "bent");
    let mut compared = 0;
    for lx in tile..n {
        let rx = lx - tile;
        if lx + reach >= n || rx < reach {
            continue;
        }
        for y in reach..n - reach {
            let l = left.values[(y * n + lx) as usize];
            let r = right.values[(y * n + rx) as usize];
            assert_eq!(l.to_bits(), r.to_bits(), "seam at x={lx} y={y}: {l} vs {r}");
            compared += 1;
        }
    }
    assert!(compared > 0);
}

#[test]
fn dem_warp_contour_builds_and_draws() {
    let json = doc(
        r##""bent": { "op": "warp", "input": "@dem", "type": "perlin",
                      "scale-px": 9, "amp-px": 4, "seed": 3 },
            "iso": { "op": "contour", "field": "@bent", "interval": 10 },
            "out": { "op": "stroke", "features": "@iso", "color": "#ff0000", "width-px": 1 }"##,
    );
    build_graph(&Document::from_json(&json).unwrap(), &default_registry()).expect("build");
    let slope = field(None, |x, _| x as f32 * 2.0);
    let r = render_with_scalar_fields(&json, SIZE, PAD, TILE, &[("terrain", slope)]);
    assert!(
        r.pixels.chunks(4).any(|p| p[3] > 0),
        "the warped isolines are drawn"
    );
}
