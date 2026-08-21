//! `density` — kernel-density fields from point features, checked through
//! a `color-ramp` grayscale mapping (the field itself isn't a renderable
//! output, so tests read it back as pixel intensity).

mod common;
use common::render_with_features;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

/// A single point feature at extent coords `(x, y)`, with optional
/// properties.
fn point_feature(props: &[(&str, Value)], x: i32, y: i32) -> Feature {
    let mut properties = HashMap::new();
    for (k, v) in props {
        properties.insert(k.to_string(), v.clone());
    }
    let mut geometry = Geometry::default();
    geometry.points.push((x, y));
    Feature {
        id: None,
        geometry,
        properties,
    }
}

fn layer(features: Vec<Feature>) -> FeatureLayer {
    FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features,
    }
}

/// A `features → density → color-ramp` doc mapping density 0..`top` to
/// black..white; `extra` is spliced into the density node's fields.
fn doc(radius: f64, top: f64, extra: &str) -> String {
    format!(
        r##"{{
          "name": "density-test",
          "tile-size": 32,
          "sources": {{
            "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }}
          }},
          "nodes": {{
            "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
            "dens":  {{ "op": "density", "features": "@feats", "radius": {radius}{extra} }},
            "out":   {{ "op": "color-ramp", "field": "@dens",
                        "stops": [ {{ "value": 0, "color": "#000000" }},
                                   {{ "value": {top}, "color": "#ffffff" }} ] }}
          }},
          "output": "@out"
        }}"##
    )
}

/// Pixels are opaque grayscale (premultiplied alpha 255), so the red
/// channel reads the ramped density directly.
fn gray(r: &ezu_graph::RasterBuf, x: u32, y: u32) -> u8 {
    let p = r.pixel(x, y);
    assert_eq!(p[3], 255, "ramp output should be opaque at ({x},{y})");
    p[0]
}

#[test]
fn single_point_yields_a_radial_monotone_field() {
    // One point at extent (2048, 2048) → canvas px (16, 16) on a 32px
    // tile. The kernel peaks there and decays monotonically to zero at
    // `radius` px.
    let l = layer(vec![point_feature(&[], 2048, 2048)]);
    let json = doc(10.0, 0.4, "");
    let r = render_with_features(&json, 32, 0, TileId { z: 0, x: 0, y: 0 }, &[("src.pts", l)]);

    // Peak near the point: GAUSS_COEF·exp(-0.5·9·(0.707/10)²) ≈ 0.390,
    // → ≈ 249 on a 0..0.4 ramp.
    let center = gray(&r, 16, 16);
    assert!(center > 235, "peak should be near 249, got {center}");
    // Monotone non-increasing along the +x ray, strictly lower mid-way.
    let mut prev = center;
    for x in 17..27 {
        let v = gray(&r, x, 16);
        assert!(
            v <= prev,
            "field must decay along +x: {v} > {prev} at x={x}"
        );
        prev = v;
    }
    assert!(
        gray(&r, 21, 16) < center,
        "mid-radius must be below the peak"
    );
    // Support is exactly `radius`: pixel centers ≥ 10px away read zero.
    assert_eq!(gray(&r, 27, 16), 0, "beyond the kernel radius");
    assert_eq!(gray(&r, 16, 27), 0, "beyond the kernel radius (y)");
    // Radial symmetry: pixel centers 12.5 and 19.5 sit 3.5px either
    // side of the point at px 16.0, so they read equal values.
    assert_eq!(gray(&r, 12, 16), gray(&r, 19, 16));
    assert_eq!(gray(&r, 16, 12), gray(&r, 16, 19));
}

#[test]
fn overlapping_points_sum() {
    let one = layer(vec![point_feature(&[], 2048, 2048)]);
    let two = layer(vec![
        point_feature(&[], 2048, 2048),
        point_feature(&[], 2048, 2048),
    ]);
    // Ramp cap 0.8 keeps both readings in the linear range: single ≈ 124,
    // double ≈ 249.
    let json = doc(10.0, 0.8, "");
    let tile = TileId { z: 0, x: 0, y: 0 };
    let r1 = render_with_features(&json, 32, 0, tile, &[("src.pts", one)]);
    let r2 = render_with_features(&json, 32, 0, tile, &[("src.pts", two)]);
    let (v1, v2) = (gray(&r1, 16, 16) as f32, gray(&r2, 16, 16) as f32);
    assert!(v1 > 100.0, "single-point center: {v1}");
    assert!(
        (v2 - 2.0 * v1).abs() <= 3.0,
        "two coincident points must sum: single={v1} double={v2}"
    );
}

#[test]
fn weight_expr_scales_per_feature() {
    // Left point weighs 1, right point weighs 3 via `["get", "w"]`.
    let l = layer(vec![
        point_feature(&[("w", Value::Int(1))], 1024, 2048), // px (8, 16)
        point_feature(&[("w", Value::Int(3))], 3072, 2048), // px (24, 16)
    ]);
    let json = doc(6.0, 1.2, r##", "weight-expr": ["get", "w"]"##);
    let r = render_with_features(&json, 32, 0, TileId { z: 0, x: 0, y: 0 }, &[("src.pts", l)]);
    let (left, right) = (gray(&r, 8, 16) as f32, gray(&r, 24, 16) as f32);
    assert!(left > 0.0, "unweighted point should register: {left}");
    assert!(
        (right - 3.0 * left).abs() <= 4.0,
        "weight 3 must scale the field 3×: left={left} right={right}"
    );
}

#[test]
fn buffer_point_outside_the_tile_contributes_inside() {
    // A point in the MVT buffer at extent x = -256 → canvas px (-2, 16).
    // With radius 8 its kernel reaches ~6px into the tile; culling it
    // would leave a visible seam at the left border.
    let l = layer(vec![point_feature(&[], -256, 2048)]);
    let json = doc(8.0, 0.4, "");
    let r = render_with_features(&json, 32, 0, TileId { z: 0, x: 0, y: 0 }, &[("src.pts", l)]);
    assert!(
        gray(&r, 0, 16) > 0 && gray(&r, 3, 16) > 0,
        "buffer point must splat into the tile: edge={} inner={}",
        gray(&r, 0, 16),
        gray(&r, 3, 16)
    );
    assert_eq!(gray(&r, 10, 16), 0, "beyond the buffered kernel's reach");
}

#[test]
fn required_pad_grows_upstream_by_radius() {
    use ezu_graph::build_graph;
    use ezu_paint::nodes::default_registry;
    use ezu_style::Document;

    let json = doc(12.0, 1.0, "");
    let doc = Document::from_json(&json).expect("parse");
    let graph = build_graph(&doc, &default_registry()).expect("build");
    let pads = graph.compute_pad(4).expect("pads");
    let ix_of = |id: &str| {
        (0..graph.len())
            .find(|&ix| graph.node_id(ix) == id)
            .expect("node id")
    };
    // color-ramp passes the doc pad through; density adds its radius
    // bound on top for its upstream.
    assert_eq!(pads[ix_of("dens")], 4);
    assert_eq!(pads[ix_of("feats")], 16);
}

#[test]
fn radius_from_a_port_is_rejected_at_build() {
    use ezu_graph::build_graph;
    use ezu_paint::nodes::default_registry;
    use ezu_style::Document;

    // A `@node`-fed radius has no static bound, so pad can't be computed.
    let json = r##"{
      "name": "density-bad-radius",
      "tile-size": 32,
      "sources": {
        "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" }
      },
      "nodes": {
        "z":     { "op": "zoom" },
        "feats": { "op": "features", "source": "src", "layer": "pts" },
        "dens":  { "op": "density", "features": "@feats", "radius": "@z" },
        "out":   { "op": "color-ramp", "field": "@dens",
                   "stops": [ { "value": 0, "color": "#000000" },
                              { "value": 1, "color": "#ffffff" } ] }
      },
      "output": "@out"
    }"##;
    let doc = Document::from_json(json).expect("parse");
    assert!(
        build_graph(&doc, &default_registry()).is_err(),
        "port-fed radius must be rejected (no static pad bound)"
    );
}
