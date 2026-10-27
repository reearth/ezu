//! The `fonts` source node loads the style's fonts into named stacks, and
//! `text` / `text-labels` take them on their `fonts` input. It replaces the
//! label nodes' deprecated `font` source names and `font-stacks`, which made
//! each label node load its fonts itself: the two wirings draw the same, the
//! fields warn, and with both the port wins.

use crate::common::render_with_features_and_images;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::{build_graph, TileId};
use std::collections::HashMap;

fn test_font(file: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts")
        .join(file);
    format!("file:{}", path.display()).replace('\\', "/")
}

/// Absolute `file:` glyphs URL template over the vendored ezu-core test
/// range (`0-255.pbf`).
fn glyphs_url() -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ezu-core/tests/glyphs");
    format!("file:{}/{{range}}.pbf", dir.display()).replace('\\', "/")
}

/// Points labelled with `name`, each naming the font it wants in `font`.
fn layer(points: &[(&str, &str, i32, i32)]) -> FeatureLayer {
    let features = points
        .iter()
        .map(|&(name, font, x, y)| {
            let mut properties = HashMap::new();
            properties.insert("name".to_string(), Value::String(name.to_string()));
            properties.insert("font".to_string(), Value::String(font.to_string()));
            let mut geometry = Geometry::default();
            geometry.points.push((x, y));
            Feature {
                id: None,
                geometry,
                properties,
            }
        })
        .collect();
    FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features,
    }
}

/// A style over `pts` with three font sources — `latin` and `digits` (two
/// outline subsets) and `sdf` (a glyphs stack) — plus `nodes`.
fn style(nodes: &str) -> String {
    format!(
        r##"{{
      "name": "fonts-source", "tile-size": 64,
      "sources": {{
        "src":    {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "latin":  {{ "type": "font", "url": "{latin}" }},
        "digits": {{ "type": "font", "url": "{digits}" }},
        "sdf":    {{ "type": "glyphs", "url": "{glyphs}", "fontstack": "Klokantech Noto Sans Regular" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        {nodes}
      }},
      "output": "@out"
    }}"##,
        latin = test_font("NotoSans-Regular.latin.ttf"),
        digits = test_font("NotoSans-Regular.digits.ttf"),
        glyphs = glyphs_url(),
    )
}

fn points() -> FeatureLayer {
    layer(&[
        ("Ab1", "Latin", 1024, 1024),
        ("Cd2", "Digits", 3072, 1024),
        ("Ef3", "Sdf", 2048, 3072),
    ])
}

fn render(nodes: &str) -> Vec<u8> {
    render_with_features_and_images(
        &style(nodes),
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", points())],
        &[],
    )
    .pixels
    .clone()
}

fn built(nodes: &str) -> ezu_graph::Graph {
    let doc = ezu_style::Document::from_json(&style(nodes)).expect("parse");
    build_graph(&doc, &ezu_paint::nodes::default_registry()).expect("build")
}

/// The font assets the graph reads.
fn font_assets(nodes: &str) -> Vec<String> {
    built(nodes)
        .asset_inputs()
        .into_iter()
        .filter(|n| n.starts_with("file:"))
        .collect()
}

const FONTS: &str = r#""fonts": { "op": "fonts", "font-stacks": {
    "Latin": ["latin", "digits"], "Digits": ["digits", "sdf"], "Sdf": ["sdf"] } }"#;
const LABEL_EXPR: &str = r#""font-expr": ["literal", ["Sdf"]]"#;

/// The text node, with the given font wiring.
fn label(fonts: &str) -> String {
    format!(
        r##""out": {{ "op": "text", "features": "@feats", {fonts},
                     "text": ["get", "name"], "size": 14, "color": "#000000", "collide": false }}"##
    )
}

#[test]
fn a_wired_fonts_node_draws_what_the_deprecated_fields_drew() {
    // One stack for every label.
    let port = format!(
        r#"{FONTS}, {}"#,
        label(r#""fonts": "@fonts", "font": ["Latin"]"#)
    );
    let fields = label(r#""font": ["latin", "digits"]"#);
    let want = render(&fields);
    assert!(want.iter().any(|&b| b != 0), "nothing drawn");
    assert_eq!(render(&port), want);

    // A stack picked per feature, an unknown one falling back to the
    // default.
    let pick = r#""font-expr": ["match", ["get", "font"], "Digits", ["literal", ["Digits"]], "Sdf", ["literal", ["Sdf"]], ["literal", ["Nowhere"]]]"#;
    let port = format!(
        r#"{FONTS}, {}"#,
        label(&format!(r#""fonts": "@fonts", "font": ["Latin"], {pick}"#))
    );
    let fields = label(&format!(
        r#""font": ["latin", "digits"], {pick}, "font-stacks": {{
            "Latin": ["latin", "digits"], "Digits": ["digits", "sdf"], "Sdf": ["sdf"] }}"#
    ));
    let want = render(&fields);
    assert_eq!(render(&port), want);
    // The stacks really differ: without `font-expr` the labels draw otherwise.
    assert_ne!(
        render(&format!(
            r#"{FONTS}, {}"#,
            label(r#""fonts": "@fonts", "font": ["Latin"]"#)
        )),
        want
    );
}

#[test]
fn a_stack_named_in_one_string_is_the_same_stack() {
    let array = format!(
        r#"{FONTS}, {}"#,
        label(r#""fonts": "@fonts", "font": ["Latin"]"#)
    );
    let string = format!(
        r#"{FONTS}, {}"#,
        label(r#""fonts": "@fonts", "font": "Latin""#)
    );
    assert_eq!(render(&string), render(&array));
}

#[test]
fn glyph_ranges_are_fetched_lazily_through_the_fonts_value() {
    // The glyphs stack starts empty: every range the labels draw is fetched
    // from the `file:` template as the label is laid out.
    let port = format!(
        r#"{FONTS}, {}"#,
        label(r#""fonts": "@fonts", "font": ["Sdf"]"#)
    );
    let ink = render(&port);
    assert!(ink.iter().any(|&b| b != 0), "the SDF labels drew nothing");
    assert_eq!(ink, render(&label(r#""font": ["sdf"]"#)));
}

#[test]
fn the_deprecated_fields_warn_with_the_fonts_node_that_replaces_them() {
    let w = built(&label(r#""font": ["latin", "digits"]"#))
        .warnings()
        .to_vec();
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].contains("`font` naming font sources is deprecated")
            && w[0].contains(
                r#"{ "op": "fonts", "font-stacks": { "latin,digits": ["latin","digits"] } }"#
            )
            && w[0].contains("`fonts`"),
        "{w:?}"
    );

    let w = built(&label(&format!(
        r#""font": ["latin"], {LABEL_EXPR}, "font-stacks": {{ "Sdf": ["sdf"] }}"#
    )))
    .warnings()
    .to_vec();
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].contains("`font` naming font sources, and `font-stacks`, are deprecated")
            && w[0].contains(r#""font-stacks": { "Sdf": ["sdf"], "latin": ["latin"] }"#),
        "{w:?}"
    );

    let port = format!(
        r#"{FONTS}, {}"#,
        label(r#""fonts": "@fonts", "font": ["Latin"]"#)
    );
    assert!(built(&port).warnings().is_empty());
}

#[test]
fn a_wired_fonts_node_wins_over_font_stacks() {
    // The ignored `font-stacks` would give `Sdf` the outline face.
    let pick = r#""font-expr": ["literal", ["Sdf"]]"#;
    let both = format!(
        r#"{FONTS}, {}"#,
        label(&format!(
            r#""fonts": "@fonts", "font": ["Latin"], {pick}, "font-stacks": {{ "Sdf": ["latin"] }}"#
        ))
    );
    let port = format!(
        r#"{FONTS}, {}"#,
        label(&format!(r#""fonts": "@fonts", "font": ["Latin"], {pick}"#))
    );
    assert_eq!(render(&both), render(&port));
    let w = built(&both).warnings().to_vec();
    assert!(
        w.len() == 1 && w[0].contains("`font-stacks` is deprecated, and ignored"),
        "{w:?}"
    );
}

#[test]
fn the_fonts_node_lists_what_it_loads() {
    let port = format!(
        r#"{FONTS}, {}"#,
        label(r#""fonts": "@fonts", "font": ["Latin"]"#)
    );
    let assets = font_assets(&port);
    assert_eq!(assets.len(), 3, "{assets:?}");
    assert!(assets.iter().any(|a| a.contains("{range}")), "{assets:?}");
    // The label node itself lists none.
    let graph = built(&port);
    let out = graph.index_of("out").expect("out");
    assert!(graph.node(out).asset_inputs().is_empty());
}

#[test]
fn a_font_the_fonts_node_lacks_fails_the_render() {
    let doc = ezu_style::Document::from_json(&style(&format!(
        r#"{FONTS}, {}"#,
        label(r#""fonts": "@fonts", "font": ["Nowhere"]"#)
    )))
    .expect("parse");
    let graph = build_graph(&doc, &ezu_paint::nodes::default_registry()).expect("build");
    let cache = ezu_graph::Cache::new();
    let bank = ezu_paint::host::BrushBankLoader::new();
    let mut loader = ezu_paint::host::TileLoader::new(&bank, TileId { z: 0, x: 0, y: 0 });
    loader.bind_features("src.pts".to_string(), points());
    let ev = ezu_graph::Evaluator::new(&graph, &cache, &loader);
    let Err(err) = ev.render(
        TileId { z: 0, x: 0, y: 0 },
        ezu_graph::CanvasInfo::square(64, 0),
        &ezu_graph::ParamValues::new(),
        0,
    ) else {
        panic!("rendered with an undeclared stack");
    };
    assert!(err.to_string().contains("`Nowhere`"), "{err}");
}
