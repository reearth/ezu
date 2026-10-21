//! `circles` paint node: constant and data-driven per-feature disks, plus
//! color-conversion parity between a constant `color` and an equivalent
//! opaque `color-expr`.

use crate::common::render_with_features;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

/// A single point feature at extent coords `(x, y)` with property `c`.
fn point_feature(c: &str, x: i32, y: i32) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("c".to_string(), Value::String(c.to_string()));
    let mut geometry = Geometry::default();
    geometry.points.push((x, y));
    Feature {
        id: None,
        geometry,
        properties,
    }
}

fn points_layer(name: &str, feats: Vec<Feature>) -> FeatureLayer {
    FeatureLayer {
        name: name.to_string(),
        extent: 4096,
        features: feats,
    }
}

#[test]
fn constant_circle_paints_red_at_point_and_clear_away() {
    // One point at tile center (extent 2048/4096 → px 32 of 64). A constant
    // red circle paints red pixels there and transparent far away.
    let layer = points_layer("pts", vec![point_feature("x", 2048, 2048)]);

    let json = r##"{
      "name": "circle-const",
      "tile-size": 64,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "pts" },
        "out":   { "op": "circles", "features": "@feats", "radius": 8, "color": "#ff0000" }
      },
      "output": "@out"
    }"##;

    let r = render_with_features(
        json,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer)],
    );

    let center = r.pixel(32, 32);
    assert!(
        center[0] > 150 && center[1] < 80 && center[2] < 80 && center[3] > 150,
        "center should be red: {center:?}"
    );
    let corner = r.pixel(2, 2);
    assert!(corner[3] < 20, "corner should be transparent: {corner:?}");
}

#[test]
fn color_expr_paints_two_points_their_classes() {
    // Two points: `c=a` (left) red, `c=b` (right) green, via a match color-expr.
    let layer = points_layer(
        "pts",
        vec![
            point_feature("a", 1024, 2048),
            point_feature("b", 3072, 2048),
        ],
    );

    let json = r##"{
      "name": "circle-dd",
      "tile-size": 64,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "pts" },
        "out":   { "op": "circles", "features": "@feats", "radius": 6, "color": "#000000",
                   "color-expr": ["match", ["get","c"], "a", "#ff0000", "#00ff00"] }
      },
      "output": "@out"
    }"##;

    let r = render_with_features(
        json,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer)],
    );

    // Left point ~px 16, right ~px 48, both at row 32.
    let left = r.pixel(16, 32);
    assert!(
        left[0] > 150 && left[1] < 80 && left[3] > 150,
        "left point should be red: {left:?}"
    );
    let right = r.pixel(48, 32);
    assert!(
        right[1] > 150 && right[0] < 80 && right[3] > 150,
        "right point should be green: {right:?}"
    );
}

#[test]
fn opaque_color_expr_matches_constant_color_pixel_for_pixel() {
    // Parity: an opaque constant `color` must paint the exact same pixels as
    // an equivalent `color-expr` at the center.
    let layer = || points_layer("pts", vec![point_feature("x", 2048, 2048)]);

    let const_json = r##"{
      "name": "const",
      "tile-size": 64,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "pts" },
        "out":   { "op": "circles", "features": "@feats", "radius": 8, "color": "#ff0000" }
      },
      "output": "@out"
    }"##;

    let expr_json = r##"{
      "name": "expr",
      "tile-size": 64,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "feats": { "op": "features", "source": "src", "layer": "pts" },
        "out":   { "op": "circles", "features": "@feats", "radius": 8, "color": "#000000",
                   "color-expr": ["match", ["get","c"], "nope", "#000000", "#ff0000"] }
      },
      "output": "@out"
    }"##;

    let tile = TileId { z: 0, x: 0, y: 0 };
    let a = render_with_features(const_json, 64, 0, tile, &[("src.pts", layer())]);
    let b = render_with_features(expr_json, 64, 0, tile, &[("src.pts", layer())]);

    assert_eq!(a.width, b.width);
    assert_eq!(a.height, b.height);
    assert_eq!(
        a.pixels, b.pixels,
        "opaque color-expr must match constant color pixel-for-pixel"
    );
    let center = a.pixel(32, 32);
    assert!(
        center[0] > 150 && center[3] > 150,
        "center should be red: {center:?}"
    );
}

/// Render one point of class `c` at the tile centre through a `circles`
/// node with `fields` (a JSON object body without braces).
fn render_one(c: &str, fields: &str) -> std::sync::Arc<ezu_graph::RasterBuf> {
    let json = format!(
        r##"{{
      "name": "circle-one",
      "tile-size": 64,
      "sources": {{ "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }} }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        "out":   {{ "op": "circles", "features": "@feats", {fields} }}
      }},
      "output": "@out"
    }}"##
    );
    render_with_features(
        &json,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[(
            "src.pts",
            points_layer("pts", vec![point_feature(c, 2048, 2048)]),
        )],
    )
}

/// For each `(class, literal)`, the expression render of that class equals
/// the literal render, and the literals really differ from one another.
fn assert_matches_literals(expr_fields: &str, literal_fields: &[(&str, &str)]) {
    let mut seen: Vec<Vec<u8>> = Vec::new();
    for (c, lit) in literal_fields {
        let want = render_one(c, lit).pixels.clone();
        assert_eq!(
            render_one(c, expr_fields).pixels,
            want,
            "class `{c}` should render as `{lit}`"
        );
        assert!(
            !seen.contains(&want),
            "the literals should differ, or the test proves nothing"
        );
        seen.push(want);
    }
}

#[test]
fn stroke_opacity_expr_renders_as_each_groups_literal() {
    assert_matches_literals(
        r##""radius": 10, "color": "#ff0000", "stroke-width": 4, "stroke-color": "#0000ff",
           "stroke-opacity-expr": ["match", ["get","c"], "faint", 0.25, 0.8]"##,
        &[
            (
                "faint",
                r##""radius": 10, "color": "#ff0000", "stroke-width": 4, "stroke-color": "#0000ff", "stroke-opacity": 0.25"##,
            ),
            (
                "strong",
                r##""radius": 10, "color": "#ff0000", "stroke-width": 4, "stroke-color": "#0000ff", "stroke-opacity": 0.8"##,
            ),
        ],
    );
}

#[test]
fn stroke_opacity_takes_the_ring_out_of_opacity() {
    // `opacity` fades the fill alone once the ring has its own opacity, as
    // `circle-opacity` and `circle-stroke-opacity` split it in MapLibre.
    let r = render_one(
        "x",
        r##""radius": 12, "color": "#ff0000", "opacity": 0.3,
            "stroke-width": 4, "stroke-color": "#0000ff", "stroke-opacity": 1"##,
    );
    let fill = r.pixel(32, 32);
    let ring = r.pixel(32 + 12, 32);
    assert!(fill[3] < 100, "the fill should be faint: {fill:?}");
    assert!(ring[3] > 240, "the ring should be opaque: {ring:?}");

    // Left out, the ring takes `opacity` as before.
    let r = render_one(
        "x",
        r##""radius": 12, "color": "#ff0000", "opacity": 0.3,
            "stroke-width": 4, "stroke-color": "#0000ff""##,
    );
    let ring = r.pixel(32 + 12, 32);
    assert!(ring[3] < 100, "the ring should be faint: {ring:?}");
}

#[test]
fn blur_expr_renders_as_each_groups_literal() {
    assert_matches_literals(
        r##""radius": 12, "color": "#ff0000",
           "blur-expr": ["match", ["get","c"], "soft", 0.5, 1]"##,
        &[
            ("soft", r##""radius": 12, "color": "#ff0000", "blur": 0.5"##),
            ("softer", r##""radius": 12, "color": "#ff0000", "blur": 1"##),
        ],
    );
}

#[test]
fn blur_fades_the_circle_towards_its_edge() {
    // With `blur: 0.5` the inner half stays at full strength and the alpha
    // falls off towards the edge.
    let r = render_one("x", r##""radius": 16, "color": "#ff0000", "blur": 0.5"##);
    let alpha = |dx: u32| r.pixel(32 + dx, 32)[3];
    assert!(
        alpha(0) > 250 && alpha(6) > 250,
        "the inner half stays solid"
    );
    assert!(
        alpha(9) > alpha(12) && alpha(12) > alpha(15),
        "the outer half fades: {} {} {}",
        alpha(9),
        alpha(12),
        alpha(15)
    );
    assert!(alpha(15) < 40, "nearly gone at the edge: {}", alpha(15));
}

#[test]
fn stroke_align_places_the_ring_against_the_radius() {
    // A red disk of radius 10 with a 4 px blue ring. Reading along the row
    // through the centre, the ring's pixels say where it sits.
    let ring_at = |align: &str| {
        let r = render_one(
            "x",
            &format!(
                r##""radius": 10, "color": "#ff0000", "stroke-width": 4,
                    "stroke-color": "#0000ff" {align}"##
            ),
        );
        let blue: Vec<u32> = (32..64)
            .filter(|&x| {
                let p = r.pixel(x, 32);
                p[3] > 200 && p[2] > 200 && p[0] < 50
            })
            .map(|x| x - 32)
            .collect();
        let red = r.pixel(32 + 9, 32);
        (blue, red)
    };
    let (centre, _) = ring_at("");
    assert_eq!(centre, ring_at(r#", "stroke-align": "center""#).0);
    assert_eq!(
        (centre[0], centre[centre.len() - 1]),
        (8, 11),
        "centred, the ring straddles the radius"
    );

    // Outside, as MapLibre draws it: radius 10 to 14, the fill kept whole.
    let (outside, red) = ring_at(r#", "stroke-align": "outside""#);
    assert_eq!((outside[0], outside[outside.len() - 1]), (10, 13));
    assert!(
        red[0] > 200 && red[2] < 50,
        "the fill reaches the radius: {red:?}"
    );

    // Inside: radius 6 to 10, so the circle stays within its radius.
    let (inside, _) = ring_at(r#", "stroke-align": "inside""#);
    assert_eq!((inside[0], inside[inside.len() - 1]), (6, 9));
}

#[test]
fn an_outside_ring_meets_its_fill_without_a_seam() {
    // Fill and ring in the same opaque colour: drawn as one disk out to the
    // ring's outer edge, every pixel well inside that edge is solid, the
    // ones the radius crosses included.
    let r = render_one(
        "x",
        r##""radius": 9.6, "color": "#ffffff", "stroke-width": 3,
            "stroke-color": "#ffffff", "stroke-align": "outside""##,
    );
    let mut thinnest = 255;
    for y in 0..64u32 {
        for x in 0..64u32 {
            let (dx, dy) = (x as f32 + 0.5 - 32.0, y as f32 + 0.5 - 32.0);
            if (dx * dx + dy * dy).sqrt() <= 9.6 + 3.0 - 1.0 {
                thinnest = thinnest.min(r.pixel(x, y)[3]);
            }
        }
    }
    assert_eq!(thinnest, 255, "a pixel on the radius shows the ground");

    // A translucent ring keeps the fill at its radius, so none of the fill
    // shows through the ring.
    let r = render_one(
        "x",
        r##""radius": 10, "color": "#ff0000", "stroke-width": 4,
            "stroke-color": "#0000ff80", "stroke-align": "outside""##,
    );
    let p = r.pixel(32 + 10, 32);
    assert_eq!(p[0], 0, "no fill under the ring: {p:?}");
}
