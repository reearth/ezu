//! `text` / `text-labels` `rotate-deg` and `rotate-deg-expr` (MapLibre
//! `text-rotate`): a point label turned about its anchor. Read through
//! `label-boxes`, whose `text` and `background` quads turn with the label and
//! whose `collision` box is their axis-aligned bounds.

use crate::common::features_of_node;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::{build_graph, RasterBuf, TileId};
use ezu_paint::nodes::{default_registry, FilteredFeatures};
use ezu_style::Document;
use std::collections::HashMap;
use std::sync::Arc;

const TILE: u32 = 128;
const UNITS_PER_PX: f32 = 4096.0 / TILE as f32;

fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

fn point(name: &str, turn: f64, x: i32, y: i32) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), Value::String(name.to_string()));
    properties.insert("turn".to_string(), Value::Double(turn));
    let mut geometry = Geometry::default();
    geometry.points.push((x, y));
    Feature {
        id: None,
        geometry,
        properties,
    }
}

/// 16 px labels through `text-labels` → `label-placement`, drawn by
/// `text-draw` over their `label-boxes`. `extra` adds fields to the label
/// layer.
fn recipe(extra: &str) -> String {
    format!(
        r##"{{
      "name": "rotate",
      "tile-size": {TILE},
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats":  {{ "op": "features", "source": "src", "layer": "pts" }},
        "labels": {{ "op": "text-labels", "features": "@feats",
                     "font": ["body"], "text": ["get", "name"], "size": 16,
                     "color": "#ffffff" {extra} }},
        "placed": {{ "op": "label-placement", "labels": ["@labels"] }},
        "boxes":  {{ "op": "label-boxes", "placement": "@placed" }},
        "fill":   {{ "op": "fill-solid", "features": "@boxes", "fill": "#0000ff" }},
        "text":   {{ "op": "text-draw", "labels": "@labels", "placement": "@placed" }},
        "out":    {{ "op": "stack", "layers": ["@fill", "@text"] }}
      }},
      "output": "@out"
    }}"##,
        font = font_url(),
    )
}

fn run(recipe: &str, features: Vec<Feature>) -> (Arc<FilteredFeatures>, Arc<RasterBuf>) {
    let layer = FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features,
    };
    features_of_node(
        recipe,
        TILE,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer)],
        "boxes",
    )
}

fn warnings(recipe: &str) -> Result<Vec<String>, String> {
    let doc = Document::from_json(recipe).map_err(|e| e.to_string())?;
    build_graph(&doc, &default_registry())
        .map(|g| g.warnings().to_vec())
        .map_err(|e| e.to_string())
}

/// Every emitted ring of `part`, its corners in px.
fn rings(feats: &FilteredFeatures, part: &str) -> Vec<Vec<(f32, f32)>> {
    feats
        .groups
        .iter()
        .filter(|g| {
            matches!(g.properties.get("part"), Some(maplibre_expr::Value::String(p)) if p == part)
        })
        .flat_map(|g| {
            g.polygons.iter().map(|p| {
                p.exterior
                    .iter()
                    .map(|&(x, y)| (x as f32 / UNITS_PER_PX, y as f32 / UNITS_PER_PX))
                    .collect()
            })
        })
        .collect()
}

type Box4 = (f32, f32, f32, f32);

fn bounds(ring: &[(f32, f32)]) -> Box4 {
    ring.iter().fold(
        (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
        |(x0, y0, x1, y1), &(x, y)| (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
    )
}

fn size((x0, y0, x1, y1): Box4) -> (f32, f32) {
    (x1 - x0, y1 - y0)
}

fn wide_label() -> Vec<Feature> {
    vec![point("AAAAAA", 90.0, 2048, 2048)]
}

#[test]
fn a_quarter_turn_stands_a_wide_label_on_end() {
    let (flat, _) = run(&recipe(""), wide_label());
    let (w, h) = size(bounds(&rings(&flat, "text")[0]));
    assert!(w > 2.0 * h, "unturned {w}x{h}");

    let (turned, _) = run(&recipe(r#", "rotate-deg": 90"#), wide_label());
    let text = rings(&turned, "text");
    assert_eq!(text.len(), 1);
    let (tw, th) = size(bounds(&text[0]));
    assert!((tw - h).abs() < 0.1 && (th - w).abs() < 0.1, "{tw}x{th}");
    // The turn is about the anchor, here the label's centre.
    let (x0, y0, x1, y1) = bounds(&text[0]);
    assert!(((x0 + x1) / 2.0 - 64.0).abs() < 0.5 && ((y0 + y1) / 2.0 - 64.0).abs() < 1.0);
    // The collision box stands on end too: the turned box, padded by 2 px.
    let collision = rings(&turned, "collision");
    assert_eq!(collision.len(), 1);
    let (cw, ch) = size(bounds(&collision[0]));
    assert!((cw - th.min(tw) - 4.0).abs() < 0.1, "{cw}x{ch}");
    assert!((ch - th.max(tw) - 4.0).abs() < 0.1, "{cw}x{ch}");
}

#[test]
fn an_oblique_turn_gives_turned_quads_and_their_bounds_to_collide_with() {
    let (turned, _) = run(
        &recipe(r#", "rotate-deg": 30, "background-padding": [2, 4, 2, 4]"#),
        wide_label(),
    );
    for part in ["text", "background"] {
        let quad = &rings(&turned, part)[0];
        // A turned rectangle: four distinct x and four distinct y.
        let mut xs: Vec<i32> = quad.iter().map(|p| (p.0 * 4.0).round() as i32).collect();
        xs.sort();
        xs.dedup();
        assert_eq!(xs.len(), 4, "{part}: {quad:?}");
        // Its top edge turns 30° clockwise.
        let (a, b) = (quad[0], quad[1]);
        let angle = libm::atan2f(b.1 - a.1, b.0 - a.0).to_degrees();
        assert!((angle - 30.0).abs() < 0.5, "{part}: {angle}");
    }
    // The collision box is the axis-aligned bounds of the turned background
    // box padded by `padding-px`, which holds the turned quad inside it.
    let bg = bounds(&rings(&turned, "background")[0]);
    let c = bounds(&rings(&turned, "collision")[0]);
    assert!(
        c.0 <= bg.0 && c.1 <= bg.1 && c.2 >= bg.2 && c.3 >= bg.3,
        "{c:?} {bg:?}"
    );
    let turn = 30f32.to_radians();
    let slack = 2.0 * (libm::cosf(turn) + libm::sinf(turn));
    for (outer, inner) in [
        (bg.0 - c.0, 0),
        (bg.1 - c.1, 1),
        (c.2 - bg.2, 2),
        (c.3 - bg.3, 3),
    ] {
        assert!(
            (outer - slack).abs() < 0.1,
            "side {inner}: {outer} vs {slack}"
        );
    }
}

#[test]
fn turned_labels_collide_by_their_turned_boxes() {
    // Two wide labels side by side, closer than one label's width: flat they
    // overlap and one drops; stood on end they pass each other.
    let feats = || {
        vec![
            point("AAAAAA", 90.0, 1536, 2048),
            point("AAAAAA", 90.0, 2560, 2048),
        ]
    };
    let (flat, _) = run(&recipe(""), feats());
    assert_eq!(rings(&flat, "text").len(), 1);
    let (turned, _) = run(&recipe(r#", "rotate-deg": 90"#), feats());
    assert_eq!(rings(&turned, "text").len(), 2);
}

#[test]
fn the_expression_turns_each_group_as_the_constant_does() {
    let constant = run(&recipe(r#", "rotate-deg": 90"#), wide_label());
    let expr = run(
        &recipe(r#", "rotate-deg-expr": ["get", "turn"]"#),
        wide_label(),
    );
    assert_eq!(
        rings(&constant.0, "text"),
        rings(&expr.0, "text"),
        "text quads"
    );
    assert_eq!(rings(&constant.0, "collision"), rings(&expr.0, "collision"));
    assert!(constant.1.pixels == expr.1.pixels, "the renders differ");
    // The render itself turns: it differs from the flat label's.
    let flat = run(&recipe(""), wide_label());
    assert!(flat.1.pixels != constant.1.pixels);
}

#[test]
fn a_zero_turn_draws_the_flat_label() {
    let feats = || vec![point("Ag", 0.0, 2048, 2048)];
    let flat = run(&recipe(r##", "background-color": "#ff0000""##), feats());
    let zero = run(
        &recipe(
            r##", "background-color": "#ff0000", "rotate-deg": 0,
                  "rotate-deg-expr": ["get", "turn"]"##,
        ),
        feats(),
    );
    assert_eq!(rings(&flat.0, "background"), rings(&zero.0, "background"));
    assert!(flat.1.pixels == zero.1.pixels, "the renders differ");
}

#[test]
fn line_placement_ignores_it_with_a_warning() {
    let w = warnings(&recipe(r#", "rotate-deg": 30, "placement": "line""#)).expect("builds");
    assert!(
        w.iter()
            .any(|w| w.contains("`rotate-deg` / `rotate-deg-expr` apply to point placement only")),
        "{w:?}"
    );
    assert!(warnings(&recipe(r#", "rotate-deg": 30"#))
        .expect("builds")
        .is_empty());
}
