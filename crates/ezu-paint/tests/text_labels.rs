//! `text` node: point-placed labels shaped via the ezu-core `text`
//! module, loaded from a `font` source with a `file:` URL.

mod common;
use common::render_with_features_and_images;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

/// Absolute `file:` URL of the ezu-core test font (forward slashes so
/// the path embeds into JSON verbatim on every platform).
fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

/// Absolute `file:` glyphs URL template over the vendored ezu-core test
/// range (`0-255.pbf` — see ../ezu-core/tests/glyphs/README.md).
fn glyphs_url() -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ezu-core/tests/glyphs");
    format!("file:{}/{{range}}.pbf", dir.display()).replace('\\', "/")
}

/// A single point feature at extent coords `(x, y)` with a `name`.
fn point_feature(name: &str, x: i32, y: i32) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), Value::String(name.to_string()));
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

fn render(recipe: &str, layer: FeatureLayer) -> std::sync::Arc<ezu_graph::RasterBuf> {
    render_with_features_and_images(
        recipe,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer)],
        &[],
    )
}

fn opaque_in(r: &ezu_graph::RasterBuf, x_lo: u32, x_hi: u32) -> usize {
    let mut n = 0;
    for y in 0..r.height {
        for x in x_lo..x_hi {
            if r.pixel(x, y)[3] > 100 {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn label_renders_near_its_anchor_point() {
    let recipe = format!(
        r##"{{
      "name": "text-basic",
      "tile-size": 64,
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        "out":   {{ "op": "text", "features": "@feats", "font": ["body"],
                    "text": "WWW", "size": 20 }}
      }},
      "output": "@out"
    }}"##,
        font = font_url()
    );
    let r = render(&recipe, layer(vec![point_feature("x", 2048, 2048)]));
    // Center-anchored on the tile center: ink lands in the middle band
    // of the canvas and nowhere near the top edge.
    let mut central = 0;
    for y in 24..40 {
        for x in 8..56 {
            if r.pixel(x, y)[3] > 100 {
                central += 1;
            }
        }
    }
    assert!(central > 30, "expected label ink near center: {central}");
    let mut top = 0;
    for y in 0..8 {
        for x in 0..64 {
            if r.pixel(x, y)[3] > 100 {
                top += 1;
            }
        }
    }
    assert_eq!(top, 0, "no ink near the top edge for a centered label");
}

#[test]
fn color_and_halo_exprs_match_their_constants() {
    let base = |paint_fields: &str| {
        format!(
            r##"{{
          "name": "text-parity",
          "tile-size": 64,
          "sources": {{
            "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
            "body": {{ "type": "font", "url": "{font}" }}
          }},
          "nodes": {{
            "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
            "out":   {{ "op": "text", "features": "@feats", "font": ["body"],
                        "text": "Ag", "size": 24, {paint_fields} }}
          }},
          "output": "@out"
        }}"##,
            font = font_url(),
            paint_fields = paint_fields
        )
    };
    let constant = render(
        &base(r##""color": "#ff0000", "halo-color": "#00ff00", "halo-width": 2"##),
        layer(vec![point_feature("x", 2048, 2048)]),
    );
    let expr = render(
        &base(
            r##""color-expr": ["rgb", 255, 0, 0],
                "halo-color-expr": ["rgb", 0, 255, 0],
                "halo-width-expr": ["+", 1, 1]"##,
        ),
        layer(vec![point_feature("x", 2048, 2048)]),
    );
    assert_eq!(
        constant.pixels, expr.pixels,
        "constant paint and equivalent expressions must render identically"
    );
}

#[test]
fn label_renders_from_a_glyphs_source() {
    // Same shape as the outline test, but the stack is a `glyphs`
    // source: ranges pull lazily from the vendored PBF at eval time.
    let recipe = format!(
        r##"{{
      "name": "text-glyphs",
      "tile-size": 64,
      "sources": {{
        "src":    {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "labels": {{ "type": "glyphs", "url": "{glyphs}",
                     "fontstack": "Klokantech Noto Sans Regular" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        "out":   {{ "op": "text", "features": "@feats", "font": ["labels"],
                    "text": "WWW", "size": 20 }}
      }},
      "output": "@out"
    }}"##,
        glyphs = glyphs_url()
    );
    let r = render(&recipe, layer(vec![point_feature("x", 2048, 2048)]));
    let mut central = 0;
    for y in 24..40 {
        for x in 8..56 {
            if r.pixel(x, y)[3] > 100 {
                central += 1;
            }
        }
    }
    assert!(
        central > 30,
        "expected SDF label ink near center: {central}"
    );
    let mut top = 0;
    for y in 0..8 {
        for x in 0..64 {
            if r.pixel(x, y)[3] > 100 {
                top += 1;
            }
        }
    }
    assert_eq!(top, 0, "no ink near the top edge for a centered label");
}

#[test]
fn outline_font_and_glyphs_fallback_mix_in_one_stack() {
    // The outline subset covers letters only; digits fall through to
    // the glyphs source.
    let recipe = |font: &str, text: &str| {
        format!(
            r##"{{
          "name": "text-mixed",
          "tile-size": 64,
          "sources": {{
            "src":    {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
            "body":   {{ "type": "font", "url": "{font_url}" }},
            "labels": {{ "type": "glyphs", "url": "{glyphs}",
                         "fontstack": "Klokantech Noto Sans Regular" }}
          }},
          "nodes": {{
            "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
            "out":   {{ "op": "text", "features": "@feats", "font": {font},
                        "text": "{text}", "size": 24 }}
          }},
          "output": "@out"
        }}"##,
            font_url = font_url(),
            glyphs = glyphs_url(),
            font = font,
            text = text
        )
    };
    let ink = |r: &ezu_graph::RasterBuf| opaque_in(r, 0, 64);
    let feature = || layer(vec![point_feature("x", 2048, 2048)]);

    // Outline alone cannot shape a digit …
    let outline_only = render(&recipe(r#"["body"]"#, "1"), feature());
    assert_eq!(ink(&outline_only), 0, "latin subset has no digits");
    // … the glyphs fallback shapes it …
    let fallback = render(&recipe(r#"["body", "labels"]"#, "1"), feature());
    assert!(ink(&fallback) > 0, "digit must fall through to the SDF run");
    // … and a mixed label renders both runs (outline 'A' + SDF '1').
    let mixed = render(&recipe(r#"["body", "labels"]"#, "A1"), feature());
    assert!(
        ink(&mixed) > ink(&fallback),
        "mixed label should add the outline run's ink: {} vs {}",
        ink(&mixed),
        ink(&fallback)
    );
}

#[test]
fn text_expression_renders_per_feature_labels() {
    let recipe = format!(
        r##"{{
      "name": "text-dd",
      "tile-size": 64,
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        "out":   {{ "op": "text", "features": "@feats", "font": ["body"],
                    "text": ["get", "name"], "size": 16 }}
      }},
      "output": "@out"
    }}"##,
        font = font_url()
    );
    // Left point labelled "i" (hairline), right point "WWW" (wide) —
    // the per-feature expression must give the right half far more ink.
    let r = render(
        &recipe,
        layer(vec![
            point_feature("i", 1024, 2048),
            point_feature("WWW", 3072, 2048),
        ]),
    );
    let left = opaque_in(&r, 0, 32);
    let right = opaque_in(&r, 32, 64);
    assert!(left > 0, "left label should paint something: {left}");
    assert!(
        right > left * 2,
        "wide right label ({right} px) should dwarf the left one ({left} px)"
    );
}

/// A `format` (multi-section) `text-field` — as emitted for MapLibre
/// `["format", …]` labels (e.g. Protomaps' multi-script place names) —
/// builds and renders. The sections are flattened to one string, so an
/// embedded `"\n"` section stacks the label exactly like the equivalent
/// plain two-line string.
#[test]
fn formatted_text_field_renders_like_the_flattened_string() {
    let recipe = |text: &str| {
        format!(
            r##"{{
          "name": "text-formatted",
          "tile-size": 64,
          "sources": {{
            "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
            "body": {{ "type": "font", "url": "{font}" }}
          }},
          "nodes": {{
            "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
            "out":   {{ "op": "text", "features": "@feats", "font": ["body"],
                        "text": {text}, "size": 20 }}
          }},
          "output": "@out"
        }}"##,
            font = font_url(),
            text = text
        )
    };

    // `["format", "AB", {}, "\n", {}, "CD", {}]` → the sections flatten to
    // "AB\nCD"; `layout` turns the newline into a line break.
    let formatted = render(
        &recipe(r##"["format", "AB", {}, "\n", {}, "CD", {}]"##),
        layer(vec![point_feature("x", 2048, 2048)]),
    );
    let plain = render(
        &recipe(r##""AB\nCD""##),
        layer(vec![point_feature("x", 2048, 2048)]),
    );

    let formatted_ink = opaque_in(&formatted, 0, formatted.width);
    let plain_ink = opaque_in(&plain, 0, plain.width);
    assert!(
        formatted_ink > 30,
        "the formatted label should render ink, got {formatted_ink}"
    );
    assert_eq!(
        formatted_ink, plain_ink,
        "flattened `format` label ({formatted_ink} px) should match the plain two-line string ({plain_ink} px)"
    );
}
