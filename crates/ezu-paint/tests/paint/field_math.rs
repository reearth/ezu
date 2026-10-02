//! `field-math` — `math`'s functions applied to scalar fields per pixel.

use crate::common::{field_of_node, render_with_scalar_fields};
use ezu_graph::{
    build_graph, Cache, CanvasInfo, Evaluator, GeoScale, NoAssets, ParamValues, PortKind,
    ScalarField, TileId,
};
use ezu_paint::host::TileLoader;
use ezu_paint::nodes::default_registry;
use ezu_style::Document;

const TILE: TileId = TileId { z: 0, x: 0, y: 0 };
const SIZE: u32 = 8;
const PAD: u32 = 1;
const N: u32 = SIZE + 2 * PAD;

const GEO: GeoScale = GeoScale {
    metres_per_pixel_x: 12.5,
    metres_per_pixel_y: 12.5,
};

/// An `n × n` field of `f(x, y)`.
fn sized(n: u32, nodata: Option<f32>, f: impl Fn(u32, u32) -> f32) -> ScalarField {
    let mut values = Vec::with_capacity((n * n) as usize);
    for y in 0..n {
        for x in 0..n {
            values.push(f(x, y));
        }
    }
    ScalarField {
        width: n,
        height: n,
        values: values.into(),
        nodata,
        geo_scale: Some(GEO),
    }
}

/// A padded-canvas field of `f(x, y)`.
fn field(nodata: Option<f32>, f: impl Fn(u32, u32) -> f32) -> ScalarField {
    sized(N, nodata, f)
}

/// A field of one value everywhere.
fn constant(v: f32) -> ScalarField {
    field(None, move |_, _| v)
}

/// Three DEM fields (`@p`, `@q`, `@r`), a scalar `@two` and a `$k` param
/// defaulting to 3, then the node `fm` as written, drawn through a
/// `color-ramp`. An unbound DEM reads as zero.
fn doc(fm: &str) -> String {
    format!(
        r##"{{
          "name": "field-math-test",
          "tile-size": {SIZE},
          "pad": {PAD},
          "params": {{ "k": {{ "type": "number", "default": 3 }} }},
          "sources": {{
            "p": {{ "type": "dem", "url": "http://example.invalid/p/{{z}}/{{x}}/{{y}}.webp",
                    "encoding": "terrarium" }},
            "q": {{ "type": "dem", "url": "http://example.invalid/q/{{z}}/{{x}}/{{y}}.webp",
                    "encoding": "terrarium" }},
            "r": {{ "type": "dem", "url": "http://example.invalid/r/{{z}}/{{x}}/{{y}}.webp",
                    "encoding": "terrarium" }}
          }},
          "nodes": {{
            "p": {{ "op": "dem", "source": "p" }},
            "q": {{ "op": "dem", "source": "q" }},
            "r": {{ "op": "dem", "source": "r" }},
            "two": {{ "op": "math", "fn": "add", "a": 1, "b": 1 }},
            "fm": {fm},
            "out": {{ "op": "color-ramp", "field": "@fm",
                      "stops": [ {{ "value": 0, "color": "#000000" }},
                                 {{ "value": 100, "color": "#ffffff" }} ] }}
          }},
          "output": "@out"
        }}"##
    )
}

/// The field `fm` produced, with `p` and `q` bound as given.
fn run(fm: &str, p: ScalarField, q: ScalarField) -> std::sync::Arc<ScalarField> {
    field_of_node(&doc(fm), SIZE, PAD, TILE, &[("p", p), ("q", q)], "fm")
}

/// The error message from building `fm`.
fn build_error(fm: &str) -> String {
    let style = Document::from_json(&doc(fm)).expect("parse");
    match build_graph(&style, &default_registry()) {
        Ok(_) => panic!("`{fm}` built"),
        Err(e) => e.to_string(),
    }
}

/// The error message from rendering `fm` with `p` and `q` bound as given.
fn eval_error(fm: &str, p: ScalarField, q: ScalarField) -> String {
    let style = Document::from_json(&doc(fm)).expect("parse");
    let graph = build_graph(&style, &default_registry()).expect("build");
    let cache = Cache::new();
    let mut loader = TileLoader::new(&NoAssets, TILE);
    loader.bind_scalar_field("p".to_string(), p);
    loader.bind_scalar_field("q".to_string(), q);
    let result = Evaluator::new(&graph, &cache, &loader).render(
        TILE,
        CanvasInfo::square(SIZE, PAD),
        &ParamValues::new(),
        0,
    );
    match result {
        Ok(_) => panic!("`{fm}` rendered"),
        Err(e) => e.to_string(),
    }
}

/// Every value of `f`, each paired with its `(x, y)`.
fn each(f: &ScalarField) -> impl Iterator<Item = ((u32, u32), f32)> + '_ {
    f.values
        .iter()
        .enumerate()
        .map(move |(i, &v)| ((i as u32 % f.width, i as u32 / f.width), v))
}

#[test]
fn add_two_fields() {
    let out = run(
        r#"{ "op": "field-math", "fn": "add", "a": "@p", "b": "@q" }"#,
        field(None, |x, _| x as f32),
        field(None, |_, y| 10.0 * y as f32),
    );
    assert_eq!((out.width, out.height), (N, N));
    for ((x, y), v) in each(&out) {
        assert_eq!(v, x as f32 + 10.0 * y as f32, "({x}, {y})");
    }
}

#[test]
fn a_number_applies_to_every_pixel() {
    let ramp = || field(None, |x, y| (x + y) as f32);
    for b in ["2.5", "\"$k\"", "\"@two\""] {
        let k = match b {
            "2.5" => 2.5,
            "\"$k\"" => 3.0,
            _ => 2.0,
        };
        let out = run(
            &format!(r#"{{ "op": "field-math", "fn": "mul", "a": "@p", "b": {b} }}"#),
            ramp(),
            constant(0.0),
        );
        for ((x, y), v) in each(&out) {
            assert_eq!(v, (x + y) as f32 * k, "b = {b}, ({x}, {y})");
        }
    }
    // The number can come first, too.
    let out = run(
        r#"{ "op": "field-math", "fn": "sub", "a": 100, "b": "@p" }"#,
        ramp(),
        constant(0.0),
    );
    for ((x, y), v) in each(&out) {
        assert_eq!(v, 100.0 - (x + y) as f32);
    }
}

#[test]
fn lerp_mixes_by_a_field_per_pixel() {
    // `c` is 0 on the left column, 1 on the right, 0.5 in between.
    let t = |x: u32| match x {
        0 => 0.0,
        x if x == N - 1 => 1.0,
        _ => 0.5,
    };
    let mix = field_of_node(
        &doc(r#"{ "op": "field-math", "fn": "lerp", "a": "@p", "b": "@q", "c": "@r" }"#),
        SIZE,
        PAD,
        TILE,
        &[
            ("p", field(None, |_, y| y as f32)),
            ("q", constant(30.0)),
            ("r", field(None, move |x, _| t(x))),
        ],
        "fm",
    );
    for ((x, y), v) in each(&mix) {
        let a = y as f32;
        assert_eq!(v, a + (30.0 - a) * t(x), "({x}, {y})");
    }

    // Numbers mixed by a field.
    let mix = run(
        r#"{ "op": "field-math", "fn": "lerp", "a": 10, "b": 30, "c": "@q" }"#,
        constant(0.0),
        field(None, move |x, _| t(x)),
    );
    for ((x, _), v) in each(&mix) {
        assert_eq!(v, 10.0 + 20.0 * t(x), "x = {x}");
    }
}

#[test]
fn clamp_and_abs() {
    let signed = || field(None, |x, _| x as f32 - 5.0);
    let clamped = run(
        r#"{ "op": "field-math", "fn": "clamp", "a": "@p", "b": -2, "c": 3 }"#,
        signed(),
        constant(0.0),
    );
    for ((x, _), v) in each(&clamped) {
        assert_eq!(v, (x as f32 - 5.0).clamp(-2.0, 3.0));
    }
    // Like `math`, swapped bounds clamp to the same range.
    let swapped = run(
        r#"{ "op": "field-math", "fn": "clamp", "a": "@p", "b": 3, "c": -2 }"#,
        signed(),
        constant(0.0),
    );
    assert_eq!(swapped.values, clamped.values);

    let abs = run(
        r#"{ "op": "field-math", "fn": "abs", "a": "@p" }"#,
        signed(),
        constant(0.0),
    );
    for ((x, _), v) in each(&abs) {
        assert_eq!(v, (x as f32 - 5.0).abs());
    }
}

#[test]
fn mod_is_euclidean_like_math() {
    let out = run(
        r#"{ "op": "field-math", "fn": "mod", "a": "@p", "b": 3 }"#,
        field(None, |x, _| x as f32 - 5.0),
        constant(0.0),
    );
    for ((x, _), v) in each(&out) {
        let a = x as f64 - 5.0;
        assert_eq!(v, a.rem_euclid(3.0) as f32);
        assert!(v >= 0.0);
    }
}

#[test]
fn dividing_by_zero_leaves_the_pixel_missing() {
    // `math` refuses a non-finite result; per pixel, that pixel is lost.
    let nodata = -9999.0;
    let out = run(
        r#"{ "op": "field-math", "fn": "div", "a": "@p", "b": "@q" }"#,
        field(Some(nodata), |x, _| x as f32),
        field(None, |x, _| if x < 3 { 0.0 } else { 2.0 }),
    );
    for ((x, _), v) in each(&out) {
        if x < 3 {
            // 0/0 is NaN and 1/0, 2/0 are infinite: all missing.
            assert_eq!(v, nodata, "x = {x}");
        } else {
            assert_eq!(v, x as f32 / 2.0);
        }
    }
    // `mod` by zero is missing too, written as NaN without a nodata.
    let out = run(
        r#"{ "op": "field-math", "fn": "mod", "a": "@p", "b": 0 }"#,
        constant(4.0),
        constant(0.0),
    );
    assert!(out.values.iter().all(|v| v.is_nan()));
}

#[test]
fn missing_samples_propagate() {
    let nodata = -32768.0;
    // A hole at x = 2 in `p` (nodata) and at x = 5 in `q` (NaN).
    let p = field(Some(nodata), |x, _| if x == 2 { nodata } else { 1.0 });
    let q = field(None, |x, _| if x == 5 { f32::NAN } else { 2.0 });
    for (a, b) in [("@p", "@q"), ("@q", "@p")] {
        let out = run(
            &format!(r#"{{ "op": "field-math", "fn": "add", "a": "{a}", "b": "{b}" }}"#),
            p.clone(),
            q.clone(),
        );
        for ((x, _), v) in each(&out) {
            match (x, a) {
                // The output writes its first field's nodata, or NaN.
                (2 | 5, "@p") => assert_eq!(v, nodata, "x = {x}"),
                (2 | 5, _) => assert!(v.is_nan(), "x = {x}"),
                _ => assert_eq!(v, 3.0),
            }
        }
    }
}

#[test]
fn sqrt_of_a_negative_is_missing() {
    let out = run(
        r#"{ "op": "field-math", "fn": "sqrt", "a": "@p" }"#,
        field(Some(-1.0), |x, _| x as f32 - 4.0),
        constant(0.0),
    );
    for ((x, _), v) in each(&out) {
        if x < 4 {
            assert_eq!(v, -1.0, "x = {x}");
        } else {
            assert_eq!(v, (x as f32 - 4.0).sqrt());
        }
    }
}

#[test]
fn the_first_field_sets_nodata_and_geo_scale() {
    let other = GeoScale {
        metres_per_pixel_x: 3.0,
        metres_per_pixel_y: 4.0,
    };
    let p = field(Some(-1.0), |_, _| 1.0);
    let mut q = field(Some(-2.0), |_, _| 2.0);
    q.geo_scale = Some(other);

    // `a` is a number, so `b` is the first field.
    let out = run(
        r#"{ "op": "field-math", "fn": "clamp", "a": 0, "b": "@q", "c": "@p" }"#,
        p.clone(),
        q.clone(),
    );
    assert_eq!(out.nodata, Some(-2.0));
    assert_eq!(out.geo_scale.unwrap().metres_per_pixel_y, 4.0);

    let out = run(
        r#"{ "op": "field-math", "fn": "max", "a": "@p", "b": "@q" }"#,
        p,
        q,
    );
    assert_eq!(out.nodata, Some(-1.0));
    assert_eq!(out.geo_scale.unwrap().metres_per_pixel_x, 12.5);
}

#[test]
fn fields_of_different_sizes_are_an_error() {
    let msg = eval_error(
        r#"{ "op": "field-math", "fn": "add", "a": "@p", "b": "@q" }"#,
        constant(1.0),
        sized(N + 2, None, |_, _| 1.0),
    );
    assert!(msg.contains("same size"), "{msg}");
}

#[test]
fn at_least_one_operand_must_be_a_field() {
    // Only literals and params: known when the node is built.
    let msg = build_error(r#"{ "op": "field-math", "fn": "mul", "a": 2, "b": "$k" }"#);
    assert!(msg.contains("scalar-field operand"), "{msg}");
    // A scalar `@node` is a port, but the graph knows its kind.
    let msg = build_error(r#"{ "op": "field-math", "fn": "mul", "a": "@two", "b": "$k" }"#);
    assert!(msg.contains("scalar-field operand"), "{msg}");
}

#[test]
fn an_unknown_fn_is_an_error() {
    let msg = build_error(r#"{ "op": "field-math", "fn": "hypot", "a": "@p", "b": "@q" }"#);
    assert!(msg.contains("unknown fn"), "{msg}");
}

#[test]
fn roughened_terrain_feeds_hillshade() {
    let json = r##"{
      "name": "field-math-terrain",
      "tile-size": 16,
      "pad": 1,
      "params": { "roughness": { "type": "number", "default": 4 } },
      "sources": {
        "terrain": { "type": "dem", "url": "http://example.invalid/{z}/{x}/{y}.webp",
                     "encoding": "terrarium" }
      },
      "nodes": {
        "dem": { "op": "dem" },
        "noise": { "op": "noise", "kind": "scalar", "type": "perlin", "scale-px": 4 },
        "bumps": { "op": "field-math", "fn": "mul", "a": "@noise", "b": "$roughness" },
        "rough": { "op": "field-math", "fn": "add", "a": "@dem", "b": "@bumps" },
        "out": { "op": "hillshade", "field": "@rough" }
      },
      "output": "@out"
    }"##;
    let style = Document::from_json(json).expect("parse");
    let graph = build_graph(&style, &default_registry()).expect("build");
    let rough = graph.index_of("rough").expect("rough node");
    assert_eq!(graph.output_kind(rough), PortKind::ScalarField);

    let n = 16 + 2;
    let ramp = sized(n, None, |x, _| 10.0 * x as f32);
    let shaded = render_with_scalar_fields(json, 16, 1, TILE, &[("terrain", ramp.clone())]);

    // Without the noise, a plain ramp shades flat; with it, it does not.
    let plain = json.replace(r#""a": "@dem", "b": "@bumps""#, r#""a": "@dem", "b": 0"#);
    let flat = render_with_scalar_fields(&plain, 16, 1, TILE, &[("terrain", ramp)]);
    assert_ne!(shaded.pixels, flat.pixels);
}
