//! Smoke tests for the `ScalarField` math ops: `map-range`,
//! `threshold`. The smoke tests pipe through `color-ramp` and assert on
//! rendered pixel colour; the missing-sample tests read the field the
//! node produced.

use crate::common::{field_of_node, render};
use ezu_graph::{GeoScale, ScalarField, TileId};

#[test]
fn map_range_normalises_zero_field_via_color_ramp() {
    // Unbound DEM source emits an all-zero ScalarField. Remap that
    // through [-1000, 1000] -> [0, 1] (zero lands at 0.5) and feed
    // into color-ramp; 0.5 is exactly between the red and blue
    // stops, so the result should be middling purple.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "terrain": { "type": "dem",
                     "url": "http://example.invalid/{z}/{x}/{y}.webp",
                     "encoding": "terrarium" }
      },
      "nodes": {
        "dem":   { "op": "dem", "name": "tile.terrain" },
        "norm":  { "op": "map-range", "field": "@dem",
                   "in-min": -1000, "in-max": 1000,
                   "out-min": 0, "out-max": 1, "clamp": true },
        "out":   { "op": "color-ramp", "field": "@norm",
                   "stops": [ { "value": 0, "color": "#ff0000" },
                              { "value": 1, "color": "#0000ff" } ] }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    // Halfway between red and blue: equal R and B, both around 0x80.
    assert!(
        (p[0] as i32 - 0x80).abs() < 8 && (p[2] as i32 - 0x80).abs() < 8,
        "expected mid-purple, got {p:?}"
    );
}

#[test]
fn threshold_binarises_via_color_ramp() {
    // Zero field thresholded at -100 with hard step: every pixel is
    // above -100, so output is `high` (1.0). Map 1.0 through a ramp
    // [0=red, 1=blue]; expect blue.
    let json = r##"{
      "name": "demo",
      "tile-size": 8,
      "sources": {
        "terrain": { "type": "dem",
                     "url": "http://example.invalid/{z}/{x}/{y}.webp",
                     "encoding": "terrarium" }
      },
      "nodes": {
        "dem":  { "op": "dem", "name": "tile.terrain" },
        "bin":  { "op": "threshold", "field": "@dem", "value": -100 },
        "out":  { "op": "color-ramp", "field": "@bin",
                  "stops": [ { "value": 0, "color": "#ff0000" },
                             { "value": 1, "color": "#0000ff" } ] }
      },
      "output": "@out"
    }"##;
    let r = render(json, 8, 0);
    let p = r.pixel(4, 4);
    assert_eq!(p, [0x00, 0x00, 0xff, 0xff], "got {p:?}");
}

const TILE: TileId = TileId { z: 0, x: 0, y: 0 };
const SIZE: u32 = 8;
const N: u32 = SIZE;

const GEO: GeoScale = GeoScale {
    metres_per_pixel_x: 12.5,
    metres_per_pixel_y: 12.5,
};

/// An `N × N` field of `f(x, y)`.
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
        geo_scale: Some(GEO),
    }
}

/// The field `map-range` produces from `dem` with the given fields
/// (`"in-min": 0, …`) spliced in.
fn map_range(params: &str, dem: ScalarField) -> std::sync::Arc<ScalarField> {
    let json = format!(
        r##"{{
          "name": "map-range-test",
          "tile-size": {SIZE},
          "sources": {{
            "p": {{ "type": "dem", "url": "http://example.invalid/p/{{z}}/{{x}}/{{y}}.webp",
                    "encoding": "terrarium" }}
          }},
          "nodes": {{
            "p": {{ "op": "dem", "source": "p" }},
            "mr": {{ "op": "map-range", "field": "@p", {params} }},
            "out": {{ "op": "color-ramp", "field": "@mr",
                      "stops": [ {{ "value": 0, "color": "#000000" }},
                                 {{ "value": 1, "color": "#ffffff" }} ] }}
          }},
          "output": "@out"
        }}"##
    );
    field_of_node(&json, SIZE, 0, TILE, &[("p", dem)], "mr")
}

#[test]
fn map_range_keeps_nodata_samples_missing() {
    let nodata = -9999.0;
    let dem = field(Some(nodata), |x, _| if x == 3 { nodata } else { x as f32 });
    for clamp in [false, true] {
        let out = map_range(
            &format!(r#""in-min": 0, "in-max": 10, "out-min": 0, "out-max": 1, "clamp": {clamp}"#),
            dem.clone(),
        );
        assert_eq!(out.nodata, Some(nodata));
        assert_eq!(out.geo_scale.unwrap().metres_per_pixel_x, 12.5);
        for (i, &v) in out.values.iter().enumerate() {
            let x = i as u32 % N;
            if x == 3 {
                assert_eq!(v, nodata, "x = {x}, clamp = {clamp}");
            } else {
                assert_eq!(v, x as f32 / 10.0, "x = {x}, clamp = {clamp}");
            }
        }
    }
}

#[test]
fn map_range_keeps_nan_samples_missing() {
    // NaN is missing whether or not the field has a nodata value; the
    // output writes it as that nodata, or NaN.
    let dem = |nodata| field(nodata, |x, _| if x == 5 { f32::NAN } else { x as f32 });
    // A degenerate input range would otherwise turn NaN into the midpoint.
    let params = r#""in-min": 2, "in-max": 2, "out-min": 0, "out-max": 1"#;
    let out = map_range(params, dem(None));
    assert_eq!(out.nodata, None);
    for (i, &v) in out.values.iter().enumerate() {
        let x = i as u32 % N;
        if x == 5 {
            assert!(v.is_nan(), "x = {x}");
        } else {
            assert_eq!(v, 0.5, "x = {x}");
        }
    }
    let out = map_range(params, dem(Some(-32768.0)));
    for (i, &v) in out.values.iter().enumerate() {
        let x = i as u32 % N;
        assert_eq!(v, if x == 5 { -32768.0 } else { 0.5 }, "x = {x}");
    }
}

#[test]
fn map_range_without_missing_samples_is_unchanged() {
    // The remap as it was before missing samples were kept, for fields
    // with neither nodata nor NaN: the output must match it bit for bit.
    fn before(v: f32, [in_min, in_max, out_min, out_max]: [f32; 4], clamp: bool) -> f32 {
        let span = in_max - in_min;
        let inv_span = if span.abs() < 1e-9 { 0.0 } else { 1.0 / span };
        let t = (v - in_min) * inv_span;
        let mut y = out_min + t * (out_max - out_min);
        if inv_span == 0.0 {
            y = 0.5 * (out_min + out_max);
        }
        if clamp {
            let (lo, hi) = if out_min <= out_max {
                (out_min, out_max)
            } else {
                (out_max, out_min)
            };
            y = y.clamp(lo, hi);
        }
        y
    }
    let dem = field(None, |x, y| x as f32 * 137.3 - y as f32 * 211.7 - 300.0);
    let ranges = [
        [-1000.0, 1000.0, 0.0, 1.0],
        [0.0, 500.0, 1.0, -1.0],
        [-0.3, 1.7, 0.1, 0.9],
        [5.0, 5.0, 0.0, 1.0],
    ];
    for range in ranges {
        for clamp in [false, true] {
            let [a, b, c, d] = range;
            let out = map_range(
                &format!(
                    r#""in-min": {a}, "in-max": {b}, "out-min": {c}, "out-max": {d}, "clamp": {clamp}"#
                ),
                dem.clone(),
            );
            assert_eq!(out.nodata, None);
            assert_eq!(out.geo_scale.unwrap().metres_per_pixel_y, 12.5);
            for (&v, &got) in dem.values.iter().zip(out.values.iter()) {
                assert_eq!(
                    got.to_bits(),
                    before(v, range, clamp).to_bits(),
                    "v = {v}, range = {range:?}, clamp = {clamp}"
                );
            }
        }
    }
}

/// The field `threshold` produces from `dem` with the given fields
/// (`"value": 0, …`) spliced in.
fn threshold(params: &str, dem: ScalarField) -> std::sync::Arc<ScalarField> {
    let json = format!(
        r##"{{
          "name": "threshold-test",
          "tile-size": {SIZE},
          "sources": {{
            "p": {{ "type": "dem", "url": "http://example.invalid/p/{{z}}/{{x}}/{{y}}.webp",
                    "encoding": "terrarium" }}
          }},
          "nodes": {{
            "p": {{ "op": "dem", "source": "p" }},
            "th": {{ "op": "threshold", "field": "@p", {params} }},
            "out": {{ "op": "color-ramp", "field": "@th",
                      "stops": [ {{ "value": 0, "color": "#000000" }},
                                 {{ "value": 1, "color": "#ffffff" }} ] }}
          }},
          "output": "@out"
        }}"##
    );
    field_of_node(&json, SIZE, 0, TILE, &[("p", dem)], "th")
}

/// The threshold as it was before missing samples were kept.
fn threshold_before(v: f32, value: f32, softness: f32, low: f32, high: f32) -> f32 {
    let half = softness * 0.5;
    let (lo, hi) = (value - half, value + half);
    let t = if softness <= 0.0 {
        if v <= value {
            0.0
        } else {
            1.0
        }
    } else if v <= lo {
        0.0
    } else if v >= hi {
        1.0
    } else {
        (v - lo) / softness
    };
    low + t * (high - low)
}

#[test]
fn threshold_keeps_nodata_samples_missing() {
    let nodata = -9999.0;
    let dem = field(Some(nodata), |x, _| if x == 3 { nodata } else { x as f32 });
    for softness in [0.0, 4.0] {
        let out = threshold(
            &format!(r#""value": 3.5, "softness": {softness}, "low": 0.25, "high": 0.75"#),
            dem.clone(),
        );
        assert_eq!(out.nodata, Some(nodata));
        assert_eq!(out.geo_scale.unwrap().metres_per_pixel_x, 12.5);
        for (i, &v) in out.values.iter().enumerate() {
            let x = i as u32 % N;
            let want = if x == 3 {
                nodata
            } else {
                threshold_before(x as f32, 3.5, softness, 0.25, 0.75)
            };
            assert_eq!(v, want, "x = {x}, softness = {softness}");
        }
    }
}

#[test]
fn threshold_keeps_nan_samples_missing() {
    // NaN is missing whether or not the field has a nodata value; the
    // output writes it as that nodata, or NaN. A hard step would
    // otherwise send it to `high`, a soft one to NaN.
    let dem = |nodata| field(nodata, |x, _| if x == 5 { f32::NAN } else { x as f32 });
    for softness in [0.0, 4.0] {
        let params = format!(r#""value": 3.5, "softness": {softness}"#);
        let out = threshold(&params, dem(None));
        assert_eq!(out.nodata, None);
        for (i, &v) in out.values.iter().enumerate() {
            let x = i as u32 % N;
            if x == 5 {
                assert!(v.is_nan(), "x = {x}, softness = {softness}");
            } else {
                assert_eq!(v, threshold_before(x as f32, 3.5, softness, 0.0, 1.0));
            }
        }
        let out = threshold(&params, dem(Some(-32768.0)));
        for (i, &v) in out.values.iter().enumerate() {
            let x = i as u32 % N;
            let want = if x == 5 {
                -32768.0
            } else {
                threshold_before(x as f32, 3.5, softness, 0.0, 1.0)
            };
            assert_eq!(v, want, "x = {x}, softness = {softness}");
        }
    }
}

#[test]
fn threshold_without_missing_samples_is_unchanged() {
    // Fields with neither nodata nor NaN must match the threshold as it
    // was before, bit for bit.
    let dem = field(None, |x, y| x as f32 * 137.3 - y as f32 * 211.7 - 300.0);
    let cases = [
        [-300.0, 0.0, 0.0, 1.0],
        [100.0, 500.0, 0.0, 1.0],
        [-0.3, 1200.0, 0.9, -0.4],
        [250.0, 0.0, -2.0, 7.5],
    ];
    for [value, softness, low, high] in cases {
        let out = threshold(
            &format!(r#""value": {value}, "softness": {softness}, "low": {low}, "high": {high}"#),
            dem.clone(),
        );
        assert_eq!(out.nodata, None);
        for (&v, &got) in dem.values.iter().zip(out.values.iter()) {
            assert_eq!(
                got.to_bits(),
                threshold_before(v, value, softness, low, high).to_bits(),
                "v = {v}, value = {value}, softness = {softness}"
            );
        }
    }
}
