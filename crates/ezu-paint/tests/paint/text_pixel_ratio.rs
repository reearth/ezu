//! Labels on a denser canvas: a retina tile places exactly the labels the
//! plain one does, in the same spots, and draws them larger by the ratio.

use crate::common::features_of_node_on;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::{CanvasInfo, RasterBuf, TileId};
use ezu_paint::nodes::FilteredFeatures;
use std::collections::HashMap;

const TILE: u32 = 64;

fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

fn feature(name: &str, geometry: Geometry) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), Value::String(name.to_string()));
    Feature {
        id: None,
        geometry,
        properties,
    }
}

fn point(name: &str, x: i32, y: i32) -> Feature {
    let mut g = Geometry::default();
    g.points.push((x, y));
    feature(name, g)
}

fn line(name: &str, pts: &[(i32, i32)]) -> Feature {
    let mut g = Geometry::default();
    g.lines.push(pts.to_vec());
    feature(name, g)
}

/// Point labels crowded enough that some lose their collision, and every
/// box placement measured, placed or not.
fn points_recipe() -> String {
    format!(
        r##"{{
      "name": "ratio",
      "tile-size": {TILE},
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats":  {{ "op": "features", "source": "src", "layer": "pts" }},
        "labels": {{ "op": "text-labels", "features": "@feats", "source": "src", "layer": "pts",
                     "font": ["body"], "text": ["get", "name"], "size": 11, "padding-px": 2,
                     "halo-width": 1.5, "halo-color": "#000000", "color": "#ffffff",
                     "background-padding": [1, 2, 1, 2], "background-radius-px": 2 }},
        "placed": {{ "op": "label-placement", "labels": ["@labels"] }},
        "boxes":  {{ "op": "label-boxes", "placement": "@placed", "include": "all" }},
        "out":    {{ "op": "text-draw", "labels": "@labels", "placement": "@placed" }}
      }},
      "output": "@out"
    }}"##,
        font = font_url(),
    )
}

/// A line label walked along a bent path.
fn line_recipe() -> String {
    format!(
        r##"{{
      "name": "ratio-line",
      "tile-size": {TILE},
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats":  {{ "op": "features", "source": "src", "layer": "pts" }},
        "labels": {{ "op": "text-labels", "features": "@feats", "source": "src", "layer": "pts",
                     "placement": "line", "spacing-px": 40,
                     "font": ["body"], "text": ["get", "name"], "size": 9, "color": "#ffffff" }},
        "placed": {{ "op": "label-placement", "labels": ["@labels"] }},
        "boxes":  {{ "op": "label-boxes", "placement": "@placed", "include": "all" }},
        "out":    {{ "op": "text-draw", "labels": "@labels", "placement": "@placed" }}
      }},
      "output": "@out"
    }}"##,
        font = font_url(),
    )
}

fn layer(features: Vec<Feature>) -> FeatureLayer {
    FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features,
    }
}

fn crowd() -> Vec<Feature> {
    vec![
        point("Alpha", 2048, 2048),
        point("Beta", 2300, 2100),
        point("Gamma", 1700, 1900),
        point("Delta", 2100, 900),
        point("Epsilon", 1000, 3000),
        point("Zeta", 1100, 3100),
        point("Eta", 3300, 3400),
    ]
}

fn run(recipe: &str, ratio: u32, features: Vec<Feature>) -> (Vec<String>, RasterBuf) {
    let canvas = CanvasInfo::square(TILE * ratio, 0).with_pixel_ratio(ratio as f32);
    let (feats, raster) = features_of_node_on(
        recipe,
        canvas,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer(features))],
        "boxes",
    );
    (describe(&feats), (*raster).clone())
}

/// Every emitted box as text — its part, name, whether it placed, and its
/// ring in extent units — sorted, so two runs compare as a whole.
fn describe(feats: &FilteredFeatures) -> Vec<String> {
    let mut out: Vec<String> = feats
        .groups
        .iter()
        .flat_map(|g| {
            let props = |k: &str| format!("{:?}", g.properties.get(k));
            g.polygons.iter().map(move |p| {
                format!(
                    "{} {} {} {:?}",
                    props("part"),
                    props("name"),
                    props("placed"),
                    p.exterior
                )
            })
        })
        .collect();
    out.sort();
    out
}

/// The inclusive bounds of every pixel with some ink.
fn ink_bounds(r: &RasterBuf) -> (u32, u32, u32, u32) {
    let mut b: Option<(u32, u32, u32, u32)> = None;
    for y in 0..r.height {
        for x in 0..r.width {
            if r.pixel(x, y)[3] > 0 {
                b = Some(match b {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    b.expect("labels drew")
}

#[test]
fn a_denser_canvas_places_the_same_point_labels_in_the_same_spots() {
    let (one, _) = run(&points_recipe(), 1, crowd());
    let (two, _) = run(&points_recipe(), 2, crowd());
    assert!(
        one.iter().any(|b| b.contains("Bool(false)")),
        "the crowd has a loser, so the comparison means something"
    );
    assert_eq!(one, two);
}

#[test]
fn a_denser_canvas_places_the_same_line_labels() {
    let path = || {
        vec![line(
            "Riverside Road",
            &[(0, 3000), (1500, 2200), (4096, 2600)],
        )]
    };
    let (one, _) = run(&line_recipe(), 1, path());
    let (two, _) = run(&line_recipe(), 2, path());
    assert!(!one.is_empty(), "the line labelled");
    assert_eq!(one, two);
}

#[test]
fn labels_draw_larger_by_the_ratio() {
    let (_, one) = run(&points_recipe(), 1, crowd());
    let (_, two) = run(&points_recipe(), 2, crowd());
    assert_eq!((two.width, two.height), (2 * one.width, 2 * one.height));
    let (a0, b0, a1, b1) = ink_bounds(&one);
    let (c0, d0, c1, d1) = ink_bounds(&two);
    // Each edge lands at twice its plain position, give or take the
    // antialiased fringe, which stays a device pixel wide.
    for (plain, dense) in [(a0, c0), (b0, d0), (a1 + 1, c1 + 1), (b1 + 1, d1 + 1)] {
        assert!(
            (2 * plain as i32 - dense as i32).abs() <= 2,
            "{plain} at 1x vs {dense} at 2x"
        );
    }
}
