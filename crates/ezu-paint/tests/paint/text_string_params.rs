//! The label node's fixed-name fields follow a `string` `$param` per render:
//! an override draws what the same value written as a literal draws, and
//! the param at its default draws what the literal default draws.

use crate::common::{features_of_node_with_params, render_with_features_sprite_and_params};
use ezu_features::{Feature, FeatureLayer, Geometry, Polygon, Value};
use ezu_graph::{build_graph, CanvasInfo, RasterBuf, ScalarValue, SpriteRect, SpriteSheet, TileId};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;
use std::collections::HashMap;

const TILE: u32 = 128;
const EXTENT: i32 = 4096;
const T0: TileId = TileId { z: 0, x: 0, y: 0 };

/// Extent coords of a tile-pixel position.
fn at(px: f32, py: f32) -> (i32, i32) {
    let u = EXTENT as f32 / TILE as f32;
    ((px * u) as i32, (py * u) as i32)
}

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

/// Something for every field to act on: a two-line label (justify), a label
/// overlapping it (overlap), a road (line placement) and a right triangle,
/// whose centroid and pole sit apart (polygon anchor).
fn features() -> FeatureLayer {
    let point = |name: &str, px: f32, py: f32| {
        let mut g = Geometry::default();
        g.points.push(at(px, py));
        feature(name, g)
    };
    let mut road = Geometry::default();
    road.lines
        .push(vec![at(4.0, 112.0), at(64.0, 110.0), at(124.0, 112.0)]);
    let mut lot = Geometry::default();
    lot.polygons.push(Polygon {
        exterior: vec![at(8.0, 60.0), at(8.0, 96.0), at(56.0, 96.0), at(8.0, 60.0)],
        holes: Vec::new(),
    });
    FeatureLayer {
        name: "pts".to_string(),
        extent: EXTENT as u32,
        features: vec![
            point("Wide words\nab", 72.0, 28.0),
            point("Ab", 76.0, 34.0),
            feature("Road", road),
            feature("Lot", lot),
        ],
    }
}

/// A sheet with one 16×16 opaque green icon, `dot`, with room to stretch.
fn dot_sheet() -> SpriteSheet {
    let mut atlas = RasterBuf::new(16, 16);
    for px in atlas.pixels.as_chunks_mut::<4>().0 {
        *px = [0, 255, 0, 255];
    }
    let mut icons = HashMap::new();
    icons.insert(
        "dot".to_string(),
        SpriteRect {
            x: 0,
            y: 0,
            width: 16,
            height: 16,
            pixel_ratio: 1.0,
            ..SpriteRect::default()
        },
    );
    SpriteSheet { atlas, icons }
}

/// One label layer with `fields` added, `params` declared.
fn doc(params: &str, fields: &str) -> String {
    format!(
        r##"{{
      "name": "text-string-params",
      "tile-size": {TILE},
      "params": {{ {params} }},
      "sources": {{
        "src":   {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body":  {{ "type": "font", "url": "{font}" }},
        "sheet": {{ "type": "sprite", "image": "builtin:atlas",
                    "index": {{ "dot": {{ "x": 0, "y": 0, "width": 16, "height": 16 }} }} }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        "icons": {{ "op": "sprite", "sprite": "@sheet" }},
        "fonts": {{ "op": "fonts", "font-stacks": {{ "body": ["body"] }} }},
        "out":   {{ "op": "text", "features": "@feats", "fonts": "@fonts",
                    "font": ["body"], "text": ["get", "name"], "size": 12,
                    "color": "#000000" {fields} }}
      }},
      "output": "@out"
    }}"##,
        font = font_url(),
    )
}

fn string(s: &str) -> ScalarValue {
    ScalarValue::String(s.into())
}

fn render(json: &str, params: &[(&str, ScalarValue)]) -> Vec<u8> {
    render_with_features_sprite_and_params(
        json,
        TILE,
        0,
        T0,
        &[("src.pts", features())],
        "atlas",
        dot_sheet(),
        params,
    )
    .pixels
    .clone()
}

fn warnings(json: &str) -> Vec<String> {
    let doc = Document::from_json(json).expect("parse");
    build_graph(&doc, &default_registry())
        .expect("build")
        .warnings()
        .to_vec()
}

/// A field `$pick` follows: the param declared with `default`.
fn param(default: &str) -> String {
    format!(r#""pick": {{ "type": "string", "default": "{default}" }}"#)
}

/// Each field, its default, another value that draws differently, and any
/// fields that value needs to show.
const ICON: &str = r#", "icon-sheet": "@icons", "icon-name": "dot""#;
const CASES: &[(&str, &str, &str, &str)] = &[
    ("placement", "point", "line", ""),
    ("placement", "point", "line-center", ""),
    ("polygon-anchor", "pole", "centroid", ""),
    ("anchor", "center", "top-left", ""),
    ("justify", "auto", "left", ""),
    ("transform", "none", "uppercase", ""),
    ("overlap", "never", "always", ""),
    ("icon-anchor", "center", "bottom", ICON),
    ("icon-text-fit", "none", "both", ICON),
];

#[test]
fn each_fixed_name_field_follows_a_string_param_per_render() {
    for &(field, default, other, extra) in CASES {
        let literal = |v: &str| render(&doc("", &format!(r#"{extra}, "{field}": "{v}""#)), &[]);
        let (lit_default, lit_other) = (literal(default), literal(other));
        assert_ne!(lit_default, lit_other, "{field}: {other} draws differently");
        let by_param = |d: &str| doc(&param(d), &format!(r#"{extra}, "{field}": "$pick""#));
        // Declared with the field's default: unset draws the default, an
        // override draws the other value.
        let json = by_param(default);
        assert!(warnings(&json).is_empty(), "{field}: {:?}", warnings(&json));
        assert_eq!(render(&json, &[]), lit_default, "{field} unset");
        assert_eq!(
            render(&json, &[("pick", string(other))]),
            lit_other,
            "{field} = {other}"
        );
        // Declared with the other value, an override back to the default.
        let json = by_param(other);
        assert_eq!(render(&json, &[]), lit_other, "{field} unset ({other})");
        assert_eq!(
            render(&json, &[("pick", string(default))]),
            lit_default,
            "{field} = {default}"
        );
    }
}

/// A literal line placement drops the point-only fields when the graph is
/// built; a param that may pick either keeps them, and each render reads
/// them only when it places points.
#[test]
fn a_param_placement_switches_the_point_only_fields_on_and_off() {
    let point_only = r##", "rotate-deg": 20, "background-color": "#ffcc00",
        "background-padding": [2, 2, 2, 2], "dedupe-key-expr": ["get", "name"],
        "dedupe-distance-px": 8, "anchor-expr": ["literal", "left"]"##;
    let literal = |v: &str| {
        render(
            &doc("", &format!(r#"{point_only}{ICON}, "placement": "{v}""#)),
            &[],
        )
    };
    let json = doc(
        &param("point"),
        &format!(r#"{point_only}{ICON}, "placement": "$pick""#),
    );
    // Either kind may be picked, so none of them is reported as ignored.
    assert!(warnings(&json).is_empty(), "{:?}", warnings(&json));
    assert_eq!(render(&json, &[]), literal("point"));
    for p in ["line", "line-center"] {
        assert_eq!(render(&json, &[("pick", string(p))]), literal(p), "{p}");
    }
    // The literal still reports what a fixed line placement ignores.
    let w = warnings(&doc("", &format!(r#"{point_only}, "placement": "line""#)));
    assert!(
        w.iter().any(|w| w.contains("`rotate-deg`")),
        "a literal line placement reports what it ignores: {w:?}"
    );
}

/// A per-group `*-expr` still takes the place of its field for the groups it
/// names, whatever the param resolves the field to.
#[test]
fn a_layout_expr_still_overrides_its_param_driven_field() {
    let expr = r#", "transform-expr": ["to-string", "lowercase"]"#;
    let json = doc(&param("none"), &format!(r#"{expr}, "transform": "$pick""#));
    let lowercase = render(&doc("", r#", "transform": "lowercase""#), &[]);
    assert_eq!(render(&json, &[("pick", string("uppercase"))]), lowercase);
}

/// `anchor-offsets` names its anchors, so an `anchor` param has nothing to
/// move there; the positions the older fields describe follow it.
#[test]
fn an_anchor_param_moves_the_positions_the_older_fields_describe() {
    let older = r#", "offset-em": [0.5, 0.5], "radial-offset": 0.5"#;
    let json = doc(&param("center"), &format!(r#"{older}, "anchor": "$pick""#));
    let literal = |a: &str| render(&doc("", &format!(r#"{older}, "anchor": "{a}""#)), &[]);
    assert_eq!(render(&json, &[]), literal("center"));
    assert_eq!(render(&json, &[("pick", string("top"))]), literal("top"));
}

/// `label-boxes` measures the labels a param-driven field laid out, so its
/// boxes move with the override too.
#[test]
fn label_boxes_follow_a_param_driven_anchor() {
    let doc = |params: &str, anchor: &str| {
        format!(
            r##"{{
          "name": "boxes",
          "tile-size": {TILE},
          "params": {{ {params} }},
          "sources": {{
            "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
            "body": {{ "type": "font", "url": "{font}" }}
          }},
          "nodes": {{
            "feats":  {{ "op": "features", "source": "src", "layer": "pts" }},
            "fonts":  {{ "op": "fonts", "font-stacks": {{ "body": ["body"] }} }},
            "labels": {{ "op": "text-labels", "features": "@feats", "fonts": "@fonts",
                         "font": ["body"], "text": ["get", "name"],
                         "size": 12, "anchor": {anchor} }},
            "placed": {{ "op": "label-placement", "labels": ["@labels"] }},
            "boxes":  {{ "op": "label-boxes", "placement": "@placed" }},
            "out":    {{ "op": "fill-solid", "features": "@boxes", "fill": "#0000ff" }}
          }},
          "output": "@out"
        }}"##,
            font = font_url(),
        )
    };
    let boxes = |json: &str, params: &[(&str, ScalarValue)]| {
        let (feats, raster) = features_of_node_with_params(
            json,
            CanvasInfo::square(TILE, 0),
            T0,
            &[("src.pts", features())],
            "boxes",
            params,
        );
        let polys: Vec<String> = feats
            .groups
            .iter()
            .map(|g| format!("{:?}", g.polygons))
            .collect();
        (polys, raster.pixels.clone())
    };
    let json = doc(&param("center"), "\"$pick\"");
    let centre = boxes(&doc("", "\"center\""), &[]);
    let top_left = boxes(&doc("", "\"top-left\""), &[]);
    assert_ne!(centre.0, top_left.0);
    assert_eq!(boxes(&json, &[]), centre);
    assert_eq!(boxes(&json, &[("pick", string("top-left"))]), top_left);
}
