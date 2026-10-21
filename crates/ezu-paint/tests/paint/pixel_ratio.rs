//! Paint ops on a denser canvas. At a pixel ratio of 2 a tile is the same
//! map at twice the density, so every size a style writes in pixels covers
//! twice the device pixels, and a 2x tile shrunk by half looks like the 1x
//! tile.

use crate::common::{disk_sprite, render_at_ratio};
use ezu_features::{Feature, FeatureLayer, Geometry, Polygon};
use ezu_graph::{RasterBuf, TileId};
use std::collections::HashMap;
use std::sync::Arc;

const TILE: u32 = 64;
const ORIGIN: TileId = TileId { z: 0, x: 0, y: 0 };

fn feature(geometry: Geometry) -> Feature {
    Feature {
        id: None,
        geometry,
        properties: HashMap::new(),
    }
}

fn layer(features: Vec<Feature>) -> FeatureLayer {
    FeatureLayer {
        name: "l".to_string(),
        extent: 4096,
        features,
    }
}

/// A horizontal line across the tile through its middle.
fn hline() -> FeatureLayer {
    let mut g = Geometry::default();
    g.lines.push(vec![(0, 2048), (4096, 2048)]);
    layer(vec![feature(g)])
}

fn point(x: i32, y: i32) -> FeatureLayer {
    let mut g = Geometry::default();
    g.points.push((x, y));
    layer(vec![feature(g)])
}

/// A square from a quarter to three quarters of the tile.
fn square() -> FeatureLayer {
    let mut g = Geometry::default();
    g.polygons.push(Polygon {
        exterior: vec![
            (1024, 1024),
            (3072, 1024),
            (3072, 3072),
            (1024, 3072),
            (1024, 1024),
        ],
        holes: Vec::new(),
    });
    layer(vec![feature(g)])
}

/// A document whose output is the node `out`, given as a JSON object body,
/// over the feature layer `src.l` and an image `dot`.
fn doc(out: &str) -> String {
    format!(
        r##"{{
      "name": "ratio",
      "tile-size": {TILE},
      "sources": {{
        "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "dot": {{ "type": "image", "src": "builtin:dot" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "l" }},
        "img":   {{ "op": "image", "src": "@dot" }},
        "brush": {{ "op": "brush-solid", "width-px": 10, "color": "#000000" }},
        "out":   {{ {out} }}
      }},
      "output": "@out"
    }}"##
    )
}

fn render(out: &str, ratio: f32, feats: FeatureLayer) -> Arc<RasterBuf> {
    let sprite = disk_sprite(24, 24, 10.0, [255, 0, 0, 255]);
    render_at_ratio(
        &doc(out),
        TILE,
        0,
        ratio,
        ORIGIN,
        &[("src.l", feats)],
        &[("dot", sprite)],
    )
}

/// How many pixels down column `x` are inked at least `min` alpha.
fn rows_inked(r: &RasterBuf, x: u32, min: u8) -> usize {
    (0..r.height).filter(|&y| r.pixel(x, y)[3] >= min).count()
}

/// How many pixels along row `y` are inked at least `min` alpha.
fn cols_inked(r: &RasterBuf, y: u32, min: u8) -> usize {
    (0..r.width).filter(|&x| r.pixel(x, y)[3] >= min).count()
}

/// The alpha-weighted centre of the ink, in device px.
fn centroid(r: &RasterBuf) -> (f64, f64) {
    let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
    for y in 0..r.height {
        for x in 0..r.width {
            let a = r.pixel(x, y)[3] as f64;
            sx += a * (x as f64 + 0.5);
            sy += a * (y as f64 + 0.5);
            sw += a;
        }
    }
    assert!(sw > 0.0, "nothing was drawn");
    (sx / sw, sy / sw)
}

/// The 2x render's alpha averaged over 2x2 blocks, against the 1x render's
/// alpha: the mean absolute difference, in alpha levels.
fn downsampled_alpha_diff(one: &RasterBuf, two: &RasterBuf) -> f64 {
    assert_eq!((two.width, two.height), (one.width * 2, one.height * 2));
    let mut total = 0.0;
    for y in 0..one.height {
        for x in 0..one.width {
            let sum: u32 = [(0, 0), (1, 0), (0, 1), (1, 1)]
                .iter()
                .map(|&(dx, dy)| two.pixel(2 * x + dx, 2 * y + dy)[3] as u32)
                .sum();
            total += (sum as f64 / 4.0 - one.pixel(x, y)[3] as f64).abs();
        }
    }
    total / (one.width * one.height) as f64
}

#[test]
fn a_stroke_width_covers_twice_the_rows_at_ratio_two() {
    let out = r##""op": "stroke", "features": "@feats", "color": "#000000", "width-px": 4"##;
    let one = render(out, 1.0, hline());
    let two = render(out, 2.0, hline());
    assert_eq!(rows_inked(&one, 32, 128), 4);
    assert_eq!(rows_inked(&two, 64, 128), 8);
}

#[test]
fn a_stroke_width_expression_scales_too() {
    let out = r##""op": "stroke", "features": "@feats", "color": "#000000", "width-px": 1,
                  "width-expr": ["+", 2, 2]"##;
    assert_eq!(rows_inked(&render(out, 1.0, hline()), 32, 128), 4);
    assert_eq!(rows_inked(&render(out, 2.0, hline()), 64, 128), 8);
}

#[test]
fn a_dashed_offset_casing_at_ratio_two_shrinks_to_the_ratio_one_tile() {
    // Width, gap, offset and a pixel dash pattern all in style px: halved,
    // the dense tile draws the same bands at the same places.
    let out = r##""op": "stroke", "features": "@feats", "color": "#000000", "width-px": 2,
                  "gap-width-px": 4, "offset-px": 6, "dasharray": [8, 4]"##;
    let one = render(out, 1.0, hline());
    let two = render(out, 2.0, hline());
    let diff = downsampled_alpha_diff(&one, &two);
    assert!(diff < 1.0, "mean alpha difference {diff}");
    // And the comparison can tell: a render without the offset differs.
    let unshifted = render(
        r##""op": "stroke", "features": "@feats", "color": "#000000", "width-px": 2,
             "gap-width-px": 4, "dasharray": [8, 4]"##,
        1.0,
        hline(),
    );
    assert!(downsampled_alpha_diff(&unshifted, &two) > 10.0 * diff.max(0.1));
}

#[test]
fn a_circle_radius_and_ring_double_at_ratio_two() {
    let out = r##""op": "circles", "features": "@feats", "radius": 6, "color": "#000000""##;
    let one = render(out, 1.0, point(2048, 2048));
    let two = render(out, 2.0, point(2048, 2048));
    assert_eq!(cols_inked(&one, 32, 128), 12);
    assert_eq!(cols_inked(&two, 64, 128), 24);

    let ring = r##""op": "circles", "features": "@feats", "radius": 6, "color": "#00000000",
                   "stroke-width": 2, "stroke-align": "inside""##;
    // Two ring crossings of the centre row, each `stroke-width` wide.
    assert_eq!(
        cols_inked(&render(ring, 1.0, point(2048, 2048)), 32, 128),
        4
    );
    assert_eq!(
        cols_inked(&render(ring, 2.0, point(2048, 2048)), 64, 128),
        8
    );
}

#[test]
fn a_fill_outline_doubles_at_ratio_two() {
    let out = r##""op": "fill-solid", "features": "@feats", "fill": "#000000", "fill-alpha": 0,
                  "edge": "#000000", "edge-width": 2"##;
    let one = render(out, 1.0, square());
    let two = render(out, 2.0, square());
    // The outline crosses the middle row twice.
    assert_eq!(cols_inked(&one, 32, 128), 4);
    assert_eq!(cols_inked(&two, 64, 128), 8);
}

#[test]
fn a_fill_blur_at_ratio_two_shrinks_to_the_ratio_one_tile() {
    let out = r##""op": "fill-solid", "features": "@feats", "fill": "#000000", "blur-sigma": 3,
                  "edge-width": 0"##;
    let one = render(out, 1.0, square());
    let two = render(out, 2.0, square());
    let diff = downsampled_alpha_diff(&one, &two);
    assert!(diff < 1.5, "mean alpha difference {diff}");
}

/// The rows a brush stroke inks down the middle column, as a ratio of the
/// 2x render's to the 1x render's.
fn brush_footprint_ratio(out: &str) -> f64 {
    let one = render(out, 1.0, hline());
    let two = render(out, 2.0, hline());
    let a = rows_inked(&one, 32, 128) as f64;
    let b = rows_inked(&two, 64, 128) as f64;
    assert!(a > 0.0, "the 1x stroke inks nothing");
    b / a
}

#[test]
fn a_brush_line_is_twice_as_wide_at_ratio_two() {
    // The brush's own radius, from `brush-solid`'s width.
    let own = brush_footprint_ratio(
        r##""op": "line", "features": "@feats", "brush": "@brush",
                                        "color": "#000000", "pressure-jitter": 0"##,
    );
    assert!((1.6..=2.4).contains(&own), "footprint ratio {own}");
    // A `radius-px` override.
    let set = brush_footprint_ratio(
        r##""op": "line", "features": "@feats", "brush": "@brush",
                                        "color": "#000000", "radius-px": 4, "pressure-jitter": 0"##,
    );
    assert!((1.6..=2.4).contains(&set), "footprint ratio {set}");
}

#[test]
fn a_brush_line_at_ratio_two_shrinks_to_the_ratio_one_tile() {
    // With the pressure jitter on: it is seeded by world position, so the
    // dense tile jitters the line where the 1x tile does.
    let out = r##""op": "line", "features": "@feats", "brush": "@brush", "color": "#000000",
                  "radius-px": 3, "pressure-jitter": 0.4"##;
    let one = render(out, 1.0, hline());
    let two = render(out, 2.0, hline());
    let diff = downsampled_alpha_diff(&one, &two);
    assert!(diff < 3.0, "mean alpha difference {diff}");
}

#[test]
fn a_stamp_draws_its_sprite_twice_as_large_at_ratio_two() {
    let out = r##""op": "stamp", "features": "@feats", "image": "@img""##;
    let one = render(out, 1.0, point(2048, 2048));
    let two = render(out, 2.0, point(2048, 2048));
    // The sprite's disk is 20 px across in style px.
    let (a, b) = (cols_inked(&one, 32, 128), cols_inked(&two, 64, 128));
    assert!((19..=21).contains(&a), "1x disk {a} px across");
    assert!((38..=42).contains(&b), "2x disk {b} px across");
}

#[test]
fn a_stamp_offset_and_jitter_land_at_the_same_place_at_ratio_two() {
    let out = r##""op": "stamp", "features": "@feats", "image": "@img", "offset": [6, -4],
                  "position-jitter-px": 5"##;
    let (x1, y1) = centroid(&render(out, 1.0, point(2048, 2048)));
    let (x2, y2) = centroid(&render(out, 2.0, point(2048, 2048)));
    // A sprite is drawn on whole pixels, so each render may round it by
    // half a pixel of its own: up to a device px and a half between them.
    assert!(
        (x2 - 2.0 * x1).abs() <= 1.5 && (y2 - 2.0 * y1).abs() <= 1.5,
        "1x centre ({x1}, {y1}), 2x centre ({x2}, {y2})"
    );
    // The offset and jitter moved the stamp off the point at all.
    assert!((x1 - 32.0).abs() > 1.0 || (y1 - 32.0).abs() > 1.0);
}

/// The most pixels any one column inks at least half alpha: the height of a
/// row of stamps, wherever their centres fall.
fn tallest_column(r: &RasterBuf) -> usize {
    (0..r.width)
        .map(|x| rows_inked(r, x, 128))
        .max()
        .unwrap_or(0)
}

#[test]
fn a_line_stamp_width_and_native_size_double_at_ratio_two() {
    for out in [
        r##""op": "line-stamp", "features": "@feats", "image": "@img", "width-px": 12"##,
        r##""op": "line-stamp", "features": "@feats", "image": "@img""##,
    ] {
        let a = tallest_column(&render(out, 1.0, hline()));
        let b = tallest_column(&render(out, 2.0, hline()));
        assert!(a > 0 && b.abs_diff(2 * a) <= 1, "{a} rows, then {b}");
    }
}

#[test]
fn a_dab_fill_at_ratio_two_shrinks_to_the_ratio_one_tile() {
    // Radius and spacing in style px: the lattice is the same set of world
    // cells, jittered the same way, at either density.
    let out = r##""op": "fill-dabs", "features": "@feats", "color": "#000000", "opacity": 0.8,
                  "radius-px": 2, "spacing-px": 6, "hardness": 0.9"##;
    let one = render(out, 1.0, square());
    let two = render(out, 2.0, square());
    let diff = downsampled_alpha_diff(&one, &two);
    // The comparison can tell a different lattice apart.
    let other = render(
        r##""op": "fill-dabs", "features": "@feats", "color": "#000000", "opacity": 0.8,
             "radius-px": 2, "spacing-px": 7, "hardness": 0.9"##,
        1.0,
        square(),
    );
    let apart = downsampled_alpha_diff(&other, &two);
    assert!(
        diff * 3.0 < apart,
        "mean alpha difference {diff}, against {apart} for another lattice"
    );
}
