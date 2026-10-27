//! The `sprite` source node hands a whole sprite sheet to the ops that name
//! icons per feature. It replaces their deprecated `sprite` fields, which
//! made each op load the sheet itself: the two wirings draw the same, the
//! field warns, and with both the port wins.

use crate::common::render_with_features_and_sprite;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::{build_graph, RasterBuf, SpriteRect, SpriteSheet, TileId};
use std::collections::HashMap;

/// A point and a line, both naming icon `kind`.
fn layer(kind: &str) -> FeatureLayer {
    let mut properties = HashMap::new();
    properties.insert("kind".to_string(), Value::String(kind.to_string()));
    let mut geometry = Geometry::default();
    geometry.points.push((1024, 2048));
    geometry
        .lines
        .push(vec![(300, 500), (2000, 1800), (3900, 3500)]);
    FeatureLayer {
        name: "things".to_string(),
        extent: 4096,
        features: vec![Feature {
            id: None,
            geometry,
            properties,
        }],
    }
}

/// An atlas with an 8×8 red `red` and an 8×4 blue `blue`.
fn sheet() -> SpriteSheet {
    let mut atlas = RasterBuf::new(16, 8);
    for y in 0..8u32 {
        for x in 0..16u32 {
            let i = ((y * 16 + x) * 4) as usize;
            let color = if x < 8 {
                [255u8, 0, 0, 255]
            } else if y < 4 {
                [0, 0, 255, 255]
            } else {
                [0, 0, 0, 0]
            };
            atlas.pixels[i..i + 4].copy_from_slice(&color);
        }
    }
    let rect = |x, width, height| SpriteRect {
        x,
        y: 0,
        width,
        height,
        pixel_ratio: 1.0,
        ..SpriteRect::default()
    };
    let mut icons = HashMap::new();
    icons.insert("red".to_string(), rect(0, 8, 8));
    icons.insert("blue".to_string(), rect(8, 8, 4));
    SpriteSheet { atlas, icons }
}

/// A style over the `things` layer with `nodes` added; `@out` is drawn.
/// `other` is a second sprite source whose atlas is never bound.
fn style(nodes: &str) -> String {
    format!(
        r##"{{
      "name": "sprite-sheet", "tile-size": 64,
      "sources": {{
        "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "icons": {{ "type": "sprite", "image": "builtin:atlas",
                    "index": {{ "red":  {{"x":0,"y":0,"width":8,"height":8}},
                               "blue": {{"x":8,"y":0,"width":8,"height":4}} }} }},
        "other": {{ "type": "sprite", "image": "builtin:unbound",
                    "index": {{ "red": {{"x":0,"y":0,"width":8,"height":8}} }} }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "things" }},
        "sheet": {{ "op": "sprite", "sprite": "@icons" }},
        {nodes}
      }},
      "output": "@out"
    }}"##,
        font = font_url()
    )
}

fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

fn render(kind: &str, nodes: &str) -> Vec<u8> {
    render_with_features_and_sprite(
        &style(nodes),
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.things", layer(kind))],
        "atlas",
        sheet(),
    )
    .pixels
    .clone()
}

fn warnings(nodes: &str) -> Vec<String> {
    let doc = ezu_style::Document::from_json(&style(nodes)).expect("parse");
    let graph = build_graph(&doc, &ezu_paint::nodes::default_registry()).expect("build");
    graph.warnings().to_vec()
}

/// The sprite assets the graph reads.
fn asset_inputs(nodes: &str) -> Vec<String> {
    let doc = ezu_style::Document::from_json(&style(nodes)).expect("parse");
    let graph = build_graph(&doc, &ezu_paint::nodes::default_registry()).expect("build");
    graph
        .asset_inputs()
        .into_iter()
        .filter(|n| n.starts_with("builtin:"))
        .collect()
}

const STAMP_PORT: &str = r#""out": { "op": "stamp", "features": "@feats", "sheet": "@sheet", "name-expr": ["get", "kind"] }"#;
const STAMP_FIELD: &str = r#""out": { "op": "stamp", "features": "@feats", "sprite": "@icons", "name-expr": ["get", "kind"] }"#;
const LINE_PORT: &str = r#""out": { "op": "line-stamp", "features": "@feats", "sheet": "@sheet", "width-px": 6, "name-expr": ["get", "kind"] }"#;
const LINE_FIELD: &str = r#""out": { "op": "line-stamp", "features": "@feats", "sprite": "@icons", "width-px": 6, "name-expr": ["get", "kind"] }"#;
const TEXT_PORT: &str = r#""out": { "op": "text", "features": "@feats", "font": ["body"], "text": "Wa", "icon-sheet": "@sheet", "icon-name-expr": ["get", "kind"], "anchor": "left", "offset-em": [1, 0] }"#;
const TEXT_FIELD: &str = r#""out": { "op": "text", "features": "@feats", "font": ["body"], "text": "Wa", "icon-sprite": "@icons", "icon-name-expr": ["get", "kind"], "anchor": "left", "offset-em": [1, 0] }"#;

#[test]
fn a_wired_sheet_draws_what_the_deprecated_field_drew() {
    for (port, field) in [
        (STAMP_PORT, STAMP_FIELD),
        (LINE_PORT, LINE_FIELD),
        (TEXT_PORT, TEXT_FIELD),
    ] {
        for kind in ["red", "blue"] {
            let want = render(kind, field);
            assert!(want.iter().any(|&b| b != 0), "{field}: nothing drawn");
            assert_eq!(render(kind, port), want, "{port} ({kind})");
        }
    }
}

#[test]
fn the_deprecated_field_warns_with_the_wiring_that_replaces_it() {
    for (field, legacy, port) in [
        (STAMP_FIELD, "sprite", "sheet"),
        (LINE_FIELD, "sprite", "sheet"),
        (TEXT_FIELD, "icon-sprite", "icon-sheet"),
    ] {
        let w = warnings(field);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].contains(&format!("`{legacy}` is deprecated"))
                && w[0].contains(r#"{ "op": "sprite", "sprite": "@icons" }"#)
                && w[0].contains(&format!("`{port}`")),
            "{w:?}"
        );
    }
    for port in [STAMP_PORT, LINE_PORT, TEXT_PORT] {
        assert!(warnings(port).is_empty(), "{port}: {:?}", warnings(port));
    }
}

#[test]
fn a_wired_sheet_wins_over_the_field() {
    // `other`'s atlas is never bound: the render would fail if the field
    // were read.
    for (op, extra) in [("stamp", ""), ("line-stamp", r#", "width-px": 6"#)] {
        let both = format!(
            r#""out": {{ "op": "{op}", "features": "@feats", "sheet": "@sheet", "sprite": "@other",
                         "name-expr": ["get", "kind"]{extra} }}"#
        );
        let port = format!(
            r#""out": {{ "op": "{op}", "features": "@feats", "sheet": "@sheet",
                         "name-expr": ["get", "kind"]{extra} }}"#
        );
        assert_eq!(render("red", &both), render("red", &port), "{op}");
        let w = warnings(&both);
        assert!(w.len() == 1 && w[0].contains("ignored"), "{op}: {w:?}");
        assert_eq!(asset_inputs(&both), ["builtin:atlas"], "{op}");
    }
    let both = TEXT_PORT.replace(
        r#""icon-sheet""#,
        r#""icon-sprite": "@other", "icon-sheet""#,
    );
    assert_eq!(render("red", &both), render("red", TEXT_PORT), "text");
    let w = warnings(&both);
    assert!(w.len() == 1 && w[0].contains("ignored"), "text: {w:?}");
    assert_eq!(asset_inputs(&both), ["builtin:atlas"], "text");
}

#[test]
fn the_sprite_node_lists_the_sheet_it_loads() {
    // The sheet is the sprite node's to load; the op reading it names none.
    assert_eq!(asset_inputs(STAMP_PORT), ["builtin:atlas"]);
    assert_eq!(asset_inputs(LINE_PORT), ["builtin:atlas"]);
    assert_eq!(asset_inputs(TEXT_PORT), ["builtin:atlas"]);
    assert_eq!(asset_inputs(STAMP_FIELD), ["builtin:atlas"]);
}

#[test]
fn naming_icons_needs_a_sheet() {
    let doc = ezu_style::Document::from_json(&style(
        r#""out": { "op": "stamp", "features": "@feats", "name-expr": ["get", "kind"] }"#,
    ))
    .expect("parse");
    let Err(err) = build_graph(&doc, &ezu_paint::nodes::default_registry()) else {
        panic!("built without a sheet");
    };
    assert!(err.to_string().contains("`sheet`"), "{err}");
}
