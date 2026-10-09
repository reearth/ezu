//! `background-color` on point labels: a box behind the text, sized to the
//! laid-out block plus `background-padding`, positioned by `anchor` as a
//! whole, colliding as a whole, with `background-radius-px` corners, and
//! drawn the same through the shared `text-draw` path. The seam case lives
//! with the other straddling-label tests in `text_collision.rs`.

use crate::common::render_with_features_and_images;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::{RasterBuf, TileId};
use std::collections::HashMap;
use std::sync::Arc;

const RED: [u8; 4] = [255, 0, 0, 255];

fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

/// A point feature at extent coords labelled `name`.
fn point(name: &str, x: i32, y: i32) -> Feature {
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

/// A recipe labelling `pts` in white at 16 px; `extra` injects per-test
/// fields. `out` is a self-contained `text` node, or with `shared` the
/// `text-labels` → `label-placement` → `text-draw` chain.
fn recipe(extra: &str, shared: bool) -> String {
    let fields = format!(
        r##""features": "@feats", "font": ["body"], "text": ["get", "name"],
           "size": 16, "color": "#ffffff" {extra}"##
    );
    let nodes = if shared {
        format!(
            r#""labels": {{ "op": "text-labels", {fields} }},
               "placed": {{ "op": "label-placement", "labels": ["@labels"] }},
               "out":    {{ "op": "text-draw", "labels": "@labels", "placement": "@placed" }}"#
        )
    } else {
        format!(r#""out": {{ "op": "text", {fields} }}"#)
    };
    format!(
        r##"{{
      "name": "text-background",
      "tile-size": 64,
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font_url}" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        {nodes}
      }},
      "output": "@out"
    }}"##,
        font_url = font_url(),
    )
}

fn render(recipe: &str, features: Vec<Feature>) -> Arc<RasterBuf> {
    let layer = FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features,
    };
    render_with_features_and_images(
        recipe,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer)],
        &[],
    )
}

/// The inclusive pixel bounds `(min_x, min_y, max_x, max_y)` of every pixel
/// `keep` selects, or `None` when it selects none.
fn bounds(r: &RasterBuf, keep: impl Fn([u8; 4]) -> bool) -> Option<(u32, u32, u32, u32)> {
    let mut b: Option<(u32, u32, u32, u32)> = None;
    for y in 0..r.height {
        for x in 0..r.width {
            if keep(r.pixel(x, y)) {
                b = Some(match b {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    b
}

/// The bounds of everything drawn.
fn drawn(r: &RasterBuf) -> Option<(u32, u32, u32, u32)> {
    bounds(r, |p| p[3] > 0)
}

/// The bounds of the label's white glyphs: every pixel they lighten.
fn glyphs(r: &RasterBuf) -> Option<(u32, u32, u32, u32)> {
    bounds(r, |p| p[3] > 0 && p[1] > 0)
}

#[test]
fn a_background_fills_a_box_around_the_text_in_its_colour() {
    let feats = || vec![point("HH", 2048, 2048)];
    let plain = render(&recipe("", false), feats());
    let boxed = render(
        &recipe(
            r##", "background-color": "#ff0000", "background-padding": [3, 5, 3, 5]"##,
            false,
        ),
        feats(),
    );
    assert!(
        (0..plain.height).all(|y| (0..plain.width).all(|x| plain.pixel(x, y) != RED)),
        "no box without background-color"
    );
    let (x0, y0, x1, y1) = drawn(&boxed).expect("the box draws");
    // Every pixel of the box that the glyphs leave alone is the box colour,
    // and the glyphs sit inside it with the padding all round.
    let (gx0, gy0, gx1, gy1) = glyphs(&boxed).expect("the text draws over the box");
    // (Ink sits inside the text block; a pixel of slack covers the snap.)
    assert!(
        gx0 >= x0 + 4 && gx1 + 4 <= x1,
        "glyphs {gx0}..{gx1} in box {x0}..{x1}"
    );
    assert!(
        gy0 >= y0 + 2 && gy1 + 2 <= y1,
        "glyphs {gy0}..{gy1} in box {y0}..{y1}"
    );
    for y in y0..=y1 {
        for x in x0..=x1 {
            if !(gx0..=gx1).contains(&x) || !(gy0..=gy1).contains(&y) {
                assert_eq!(boxed.pixel(x, y), RED, "box pixel ({x}, {y})");
            }
        }
    }
}

#[test]
fn a_top_left_anchor_puts_the_box_corner_on_the_point_whatever_the_padding() {
    let feats = || vec![point("HH", 2048, 2048)];
    let mut glyph_x = Vec::new();
    for padding in ["[0, 0, 0, 0]", "[3, 6, 3, 6]", "[8, 1, 2, 10]"] {
        let r = render(
            &recipe(
                &format!(
                    r##", "anchor": "top-left", "background-color": "#ff0000",
                       "background-padding": {padding}"##
                ),
                false,
            ),
            feats(),
        );
        let (x0, y0, ..) = drawn(&r).expect("the box draws");
        assert_eq!(
            (x0, y0),
            (32, 32),
            "padding {padding}: box corner on the point"
        );
        glyph_x.push(glyphs(&r).expect("the text draws").0);
    }
    // The text moves inward by the left padding instead.
    assert!(
        glyph_x[0] < glyph_x[1] && glyph_x[1] < glyph_x[2],
        "{glyph_x:?}"
    );

    // `offset-em` moves the box corner, not just the text: 1 em at 16 px.
    let r = render(
        &recipe(
            r##", "anchor": "top-left", "offset-em": [1, 0.5], "background-color": "#ff0000",
               "background-padding": [3, 6, 3, 6]"##,
            false,
        ),
        feats(),
    );
    let (x0, y0, ..) = drawn(&r).expect("the box draws");
    assert_eq!((x0, y0), (48, 40));
}

#[test]
fn the_box_is_the_collision_box() {
    // Two labels whose text boxes clear each other with the default 2 px
    // collision padding; widening each into a box makes them overlap.
    let feats = || vec![point("I", 1536, 2048), point("I", 2560, 2048)];
    // Whether anything draws down each label's own anchor column (x = 24
    // and 40); neither box reaches the other's.
    let halves = |r: &RasterBuf| {
        let left = (0..r.height).any(|y| r.pixel(24, y)[3] > 0);
        let right = (0..r.height).any(|y| r.pixel(40, y)[3] > 0);
        (left, right)
    };
    let plain = render(&recipe("", false), feats());
    assert_eq!(halves(&plain), (true, true), "both plain labels place");
    let boxed = render(
        &recipe(
            r##", "background-color": "#ff0000", "background-padding": [0, 8, 0, 8]"##,
            false,
        ),
        feats(),
    );
    let (left, right) = halves(&boxed);
    assert!(left != right, "only one boxed label places: {left} {right}");
}

#[test]
fn a_radius_rounds_the_box_corners() {
    let feats = || vec![point("HH", 2048, 2048)];
    let boxed = |radius: &str| {
        render(
            &recipe(
                &format!(
                    r##", "anchor": "top-left", "background-color": "#ff0000",
                       "background-padding": [4, 4, 4, 4] {radius}"##
                ),
                false,
            ),
            feats(),
        )
    };
    let square = boxed("");
    let round = boxed(r#", "background-radius-px": 5"#);
    let (x0, y0, x1, y1) = drawn(&square).expect("the box draws");
    assert_eq!(square.pixel(x0, y0), RED, "a square corner is filled");
    assert_eq!(square.pixel(x1, y1), RED);
    // The rounded box covers the same span, its corners cut away.
    assert_eq!(drawn(&round), Some((x0, y0, x1, y1)));
    for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
        assert_eq!(round.pixel(x, y)[3], 0, "corner ({x}, {y}) is cut");
    }
    let mid = (x0 + x1) / 2;
    assert_eq!(
        round.pixel(mid, y0),
        RED,
        "the top edge between corners is filled"
    );

    // A radius past half the shorter side clamps to a pill, not a mess.
    let pill = boxed(r#", "background-radius-px": 1000"#);
    assert_eq!(drawn(&pill), Some((x0, y0, x1, y1)));
    let cy = (y0 + y1) / 2;
    assert_eq!(
        pill.pixel(x0, cy),
        RED,
        "the pill's left end is filled at mid-height"
    );
}

#[test]
fn the_shared_text_draw_path_draws_the_same_box() {
    let feats = || vec![point("HH", 2048, 2048)];
    let extra = r##", "background-color": "#ff0000", "background-padding": [3, 5, 3, 5],
                    "background-radius-px": 3"##;
    let whole = render(&recipe(extra, false), feats());
    let shared = render(&recipe(extra, true), feats());
    assert!(drawn(&whole).is_some());
    assert_eq!(whole.pixels, shared.pixels);
}
