//! `text` node: point-placed labels shaped via the ezu-core `text`
//! module, loaded from a `font` source with a `file:` URL.

use crate::common::render_with_features_and_images;
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

/// Absolute `file:` URL of the digits-only test font (covers `0-9`, not
/// letters) — lets a per-feature stack test prove which font shaped a label.
fn digits_font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.digits.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

/// A point feature at extent `(x, y)` with `name` and a `kind` property.
fn kinded_point(name: &str, kind: &str, x: i32, y: i32) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), Value::String(name.to_string()));
    properties.insert("kind".to_string(), Value::String(kind.to_string()));
    let mut geometry = Geometry::default();
    geometry.points.push((x, y));
    Feature {
        id: None,
        geometry,
        properties,
    }
}

/// A data-driven `font-expr` selects the stack per feature. Two features
/// carry the *same* label text and size but route to different stacks: the
/// letters-bearing label sent to the digits-only font drops all glyphs (no
/// ink), while the one sent to the latin font renders. This proves per-feature
/// stack selection *and* that the block cache is keyed by font (same text +
/// size must not reuse one stack's block for the other).
#[test]
fn font_expr_switches_stack_per_feature() {
    let recipe = format!(
        r##"{{
      "name": "text-font-expr",
      "tile-size": 64,
      "sources": {{
        "src":   {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "latin": {{ "type": "font", "url": "{latin}" }},
        "digits":{{ "type": "font", "url": "{digits}" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        "out":   {{ "op": "text", "features": "@feats", "size": 20,
                    "font": ["latin"],
                    "font-stacks": {{ "Digits": ["digits"], "Latin": ["latin"] }},
                    "font-expr": ["case", ["==", ["get", "kind"], "num"],
                                  ["literal", ["Digits"]], ["literal", ["Latin"]]],
                    "text": ["get", "name"] }}
      }},
      "output": "@out"
    }}"##,
        latin = font_url(),
        digits = digits_font_url()
    );
    // Left feature: "ab" routed to the digits font (kind=num) → no glyphs.
    // Right feature: "ab" routed to the latin font → renders.
    let feats = vec![
        kinded_point("ab", "num", 1024, 2048),
        kinded_point("ab", "text", 3072, 2048),
    ];
    let r = render(&recipe, layer(feats));
    let left = opaque_in(&r, 0, 32);
    let right = opaque_in(&r, 32, 64);
    assert_eq!(
        left, 0,
        "letters in the digits-only font must drop (no ink): {left}"
    );
    assert!(right > 20, "latin-routed label should render ink: {right}");
}

/// A `font-expr` result absent from `font-stacks` falls back to the default
/// `font` (still renders), rather than erroring or drawing nothing.
#[test]
fn font_expr_unknown_stack_falls_back() {
    let recipe = format!(
        r##"{{
      "name": "text-font-expr-fallback",
      "tile-size": 64,
      "sources": {{
        "src":   {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "latin": {{ "type": "font", "url": "{latin}" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        "out":   {{ "op": "text", "features": "@feats", "size": 20,
                    "font": ["latin"],
                    "font-stacks": {{ "Latin": ["latin"] }},
                    "font-expr": ["literal", ["Nonexistent Stack"]],
                    "text": ["get", "name"] }}
      }},
      "output": "@out"
    }}"##,
        latin = font_url()
    );
    // Label uses letters (the default latin font covers them; the digits-only
    // font would not — see the coverage split in the sibling test).
    let r = render(&recipe, layer(vec![point_feature("ab", 2048, 2048)]));
    assert!(
        opaque_in(&r, 0, r.width) > 20,
        "unknown stack should fall back to the default font and still render"
    );
}

/// A `format` section with a per-section `text-color` paints that section in
/// its own colour while the rest uses the block fill.
#[test]
fn format_section_text_color_paints_that_section() {
    let recipe = format!(
        r##"{{
      "name": "text-format-color",
      "tile-size": 64,
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        "out":   {{ "op": "text", "features": "@feats", "font": ["body"], "size": 24,
                    "color": "#000000",
                    "text": ["format", "AB", {{}}, "\n", {{}}, "CD",
                             {{"text-color": ["to-color", "#ff0000"]}}] }}
      }},
      "output": "@out"
    }}"##,
        font = font_url()
    );
    let r = render(&recipe, layer(vec![point_feature("x", 2048, 2048)]));
    // The second section is red; the first is black. A strongly-red pixel
    // must exist, and it must sit below the (black) first line.
    let mut red = 0;
    for y in 0..r.height {
        for x in 0..r.width {
            let p = r.pixel(x, y);
            if p[3] > 150 && p[0] > 180 && p[1] < 80 && p[2] < 80 {
                red += 1;
            }
        }
    }
    assert!(
        red > 15,
        "the red section should paint red pixels, got {red}"
    );
}

/// A `format` section's `text-font` selects a different registry stack: a
/// digit-only section renders only because its section font covers digits,
/// which the default (latin) font does not.
#[test]
fn format_section_font_selects_a_different_stack() {
    let recipe = |section_font: &str| {
        format!(
            r##"{{
          "name": "text-format-font",
          "tile-size": 64,
          "sources": {{
            "src":    {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
            "latin":  {{ "type": "font", "url": "{latin}" }},
            "digits": {{ "type": "font", "url": "{digits}" }}
          }},
          "nodes": {{
            "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
            "out":   {{ "op": "text", "features": "@feats", "font": ["latin"], "size": 24,
                        "font-stacks": {{ "Digits": ["digits"] }},
                        "text": ["format", "ab", {{}}, "\n", {{}}, "12", {section_font}] }}
          }},
          "output": "@out"
        }}"##,
            latin = font_url(),
            digits = digits_font_url(),
            section_font = section_font
        )
    };
    let ink = |recipe: &str| {
        opaque_in(
            &render(recipe, layer(vec![point_feature("x", 2048, 2048)])),
            0,
            64,
        )
    };
    // With the digits section font, "12" renders; without it the digits fall
    // to the latin font (no digit glyphs) and drop, so there is less ink.
    let with_digits = ink(&recipe(r##"{"text-font": ["literal", ["Digits"]]}"##));
    let without = ink(&recipe("{}"));
    assert!(
        with_digits > without + 15,
        "digit section should add ink via its own font: with={with_digits} without={without}"
    );
}

fn polygon_feature(exterior: &[(i32, i32)]) -> Feature {
    let mut geometry = Geometry::default();
    geometry.polygons.push(ezu_features::Polygon {
        exterior: exterior.to_vec(),
        holes: vec![],
    });
    Feature {
        id: None,
        geometry,
        properties: HashMap::new(),
    }
}

#[test]
fn point_placement_labels_a_polygon_at_its_pole_of_inaccessibility() {
    let recipe = format!(
        r##"{{
      "name": "text-polygon",
      "tile-size": 64,
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        "out":   {{ "op": "text", "features": "@feats", "font": ["body"],
                    "text": "W", "size": 12 }}
      }},
      "output": "@out"
    }}"##,
        font = font_url()
    );
    // An L whose centroid falls in its notch, outside the shape: the label
    // goes inside the tall west arm instead, around x = 512 of 4096.
    let l_shape = polygon_feature(&[
        (0, 0),
        (1024, 0),
        (1024, 3072),
        (4096, 3072),
        (4096, 4096),
        (0, 4096),
    ]);
    let r = render(&recipe, layer(vec![l_shape]));
    assert!(opaque_in(&r, 0, 16) > 10, "the label sits in the west arm");
    assert_eq!(opaque_in(&r, 24, 64), 0, "nothing near the centroid");

    // A polygon whose pole lies in the tile buffer, past the east edge, is
    // left to the tile that holds the pole.
    let in_buffer = polygon_feature(&[(4000, 1000), (4224, 1000), (4224, 3000), (4000, 3000)]);
    let r = render(&recipe, layer(vec![in_buffer]));
    assert_eq!(opaque_in(&r, 0, 64), 0);
}

/// The mean column and row of a raster's opaque pixels.
fn ink_centre(r: &ezu_graph::RasterBuf) -> (f64, f64) {
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
    for y in 0..r.height {
        for x in 0..r.width {
            if r.pixel(x, y)[3] > 100 {
                sx += f64::from(x);
                sy += f64::from(y);
                n += 1.0;
            }
        }
    }
    assert!(n > 0.0, "no ink");
    (sx / n, sy / n)
}

#[test]
fn polygon_anchor_centroid_labels_an_l_at_its_balance_point() {
    let recipe = |anchor: &str| {
        format!(
            r##"{{
          "name": "text-polygon-anchor",
          "tile-size": 64,
          "sources": {{
            "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
            "body": {{ "type": "font", "url": "{font}" }}
          }},
          "nodes": {{
            "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
            "out":   {{ "op": "text", "features": "@feats", "font": ["body"],
                        "text": "W", "size": 10 {anchor} }}
          }},
          "output": "@out"
        }}"##,
            font = font_url()
        )
    };
    // Three quadrants of the tile, the north-east one missing. The
    // centroid is (1707, 2389) of 4096, about (26.7, 37.3) px; the pole
    // where the L is roomiest, about (18.7, 45.3) px.
    let l = || {
        layer(vec![polygon_feature(&[
            (0, 0),
            (2048, 0),
            (2048, 2048),
            (4096, 2048),
            (4096, 4096),
            (0, 4096),
        ])])
    };
    let (px, py) = ink_centre(&render(&recipe(""), l()));
    let (qx, qy) = ink_centre(&render(&recipe(r#", "polygon-anchor": "pole""#), l()));
    assert_eq!((px, py), (qx, qy), "pole is the default");
    let (cx, cy) = ink_centre(&render(&recipe(r#", "polygon-anchor": "centroid""#), l()));
    assert!(
        cx - px > 5.0 && py - cy > 5.0,
        "the centroid label sits north-east of the pole's: pole ({px}, {py}), centroid ({cx}, {cy})"
    );
}

/// Render `tile` with the polygon layer `src.pts` overzoomed from `parent`:
/// this tile's piece and both horizontal neighbours', each cut from the
/// ancestor the way a host binds a source past its `max-zoom`.
fn render_overzoomed(
    recipe: &str,
    parent: (u8, u32, u32),
    ancestor: &FeatureLayer,
    tile: TileId,
) -> std::sync::Arc<ezu_graph::RasterBuf> {
    use ezu_features::mvt::{clip_to_descendant, DecodedTile};
    use ezu_graph::{build_graph, Cache, CanvasInfo, Evaluator, ParamValues, PortValue};
    let doc = ezu_style::Document::from_json(recipe).expect("parse");
    let graph = build_graph(&doc, &ezu_paint::nodes::default_registry()).expect("build");
    let cache = Cache::new();
    let fonts = ezu_paint::host::BrushBankLoader::new();
    let mut loader = ezu_paint::host::TileLoader::new(&fonts, tile);
    let parent = ezu_core::TileId::new(parent.0, parent.1, parent.2);
    let source = DecodedTile {
        layers: vec![ancestor.clone()],
        overzoom: 0,
    };
    for dx in -1i32..=1 {
        let x = tile.x as i32 + dx;
        let target = ezu_core::TileId::new(tile.z, x as u32, tile.y);
        if x < 0 || !parent.is_ancestor_of(target) {
            continue;
        }
        let cut = clip_to_descendant(&source, parent, target).expect("descendant");
        loader.bind_mvt_neighbor("src", dx, 0, cut);
    }
    let ev = Evaluator::new(&graph, &cache, &loader);
    match ev
        .render(tile, CanvasInfo::square(64, 16), &ParamValues::new(), 0)
        .expect("render")
    {
        PortValue::Raster(r) => r,
        other => panic!("expected raster output, got {:?}", other.kind()),
    }
}

#[test]
fn an_overzoomed_polygon_label_on_a_seam_draws_the_same_on_both_tiles() {
    let recipe = format!(
        r##"{{
      "name": "text-polygon-overzoom",
      "tile-size": 64,
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}", "max-zoom": 2 }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        "out":   {{ "op": "text", "features": "@feats", "source": "src", "layer": "pts",
                    "font": ["body"], "text": "WW", "size": 16 }}
      }},
      "output": "@out"
    }}"##,
        font = font_url()
    );
    // A z2 block centred a hair east of the line between its z3 children,
    // so the east child holds the anchor and the west child only the
    // label's western half.
    let ancestor = layer(vec![polygon_feature(&[
        (1500, 1500),
        (2600, 1500),
        (2600, 2500),
        (1500, 2500),
    ])]);
    let west = render_overzoomed(&recipe, (2, 1, 1), &ancestor, TileId { z: 3, x: 2, y: 2 });
    let east = render_overzoomed(&recipe, (2, 1, 1), &ancestor, TileId { z: 3, x: 3, y: 2 });
    // The west tile's east pad and the east tile's west interior show the
    // same strip of the world.
    let (size, pad) = (64u32, 16u32);
    let mut inked = 0;
    for y in 0..west.height {
        for i in 0..pad {
            let w = west.pixel(size + pad + i, y);
            let e = east.pixel(pad + i, y);
            assert_eq!(w, e, "seam mismatch at {i}, {y}");
            if w[3] > 100 {
                inked += 1;
            }
        }
    }
    assert!(inked > 0, "the label straddles the seam");
    assert!(opaque_in(&west, pad, size + pad) > 0, "the west half draws");
    assert!(opaque_in(&east, pad, size + pad) > 0, "the east half draws");
}
