//! `strikethrough` on point labels: a bar across each wrapped line, in the
//! label's colour, per feature through `strikethrough-expr`, for outline
//! fonts and `glyphs` stacks alike, and through the shared `text-draw` path.
//! The seam case lives with the other straddling-label tests in
//! `text_collision.rs`.

use crate::common::render_with_features_and_images;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::{RasterBuf, TileId};
use std::collections::HashMap;
use std::sync::Arc;

fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

fn glyphs_url() -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ezu-core/tests/glyphs");
    format!("file:{}/{{range}}.pbf", dir.display()).replace('\\', "/")
}

/// A point feature at extent coords with a `name` and an `abolished` flag.
fn point(name: &str, abolished: bool, x: i32, y: i32) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), Value::String(name.to_string()));
    properties.insert("abolished".to_string(), Value::Bool(abolished));
    let mut geometry = Geometry::default();
    geometry.points.push((x, y));
    Feature {
        id: None,
        geometry,
        properties,
    }
}

/// A recipe whose `out` node labels `pts` in grey from `font` (`body` is
/// the outline Latin subset, `labels` the vendored glyph range); `extra`
/// injects per-test fields. `out` is a self-contained `text` node, or with
/// `shared` the `text-labels` → `label-placement` → `text-draw` chain.
fn recipe(font: &str, extra: &str, shared: bool) -> String {
    let fields = format!(
        r##""features": "@feats", "font": ["{font}"], "text": ["get", "name"],
           "size": 24, "color": "#808080" {extra}"##
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
      "name": "text-strikethrough",
      "tile-size": 64,
      "sources": {{
        "src":    {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body":   {{ "type": "font", "url": "{font_url}" }},
        "labels": {{ "type": "glyphs", "url": "{glyphs}",
                     "fontstack": "Klokantech Noto Sans Regular" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        {nodes}
      }},
      "output": "@out"
    }}"##,
        font_url = font_url(),
        glyphs = glyphs_url(),
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

/// The rows on which two renders differ, ascending.
fn changed_rows(a: &RasterBuf, b: &RasterBuf) -> Vec<u32> {
    (0..a.height)
        .filter(|&y| (0..a.width).any(|x| a.pixel(x, y) != b.pixel(x, y)))
        .collect()
}

/// Split ascending rows into runs of consecutive rows.
fn runs(rows: &[u32]) -> Vec<Vec<u32>> {
    let mut out: Vec<Vec<u32>> = Vec::new();
    for &r in rows {
        match out.last_mut() {
            Some(run) if *run.last().unwrap() + 1 == r => run.push(r),
            _ => out.push(vec![r]),
        }
    }
    out
}

/// The rows any ink (alpha over half) touches, ascending.
fn ink_rows(r: &RasterBuf) -> Vec<u32> {
    (0..r.height)
        .filter(|&y| (0..r.width).any(|x| r.pixel(x, y)[3] > 128))
        .collect()
}

#[test]
fn strikethrough_bars_a_point_label_in_its_colour() {
    let feats = || vec![point("HHH", false, 2048, 2048)];
    let plain = render(&recipe("body", "", false), feats());
    let struck = render(
        &recipe("body", r#", "strikethrough": true"#, false),
        feats(),
    );
    let rows = changed_rows(&plain, &struck);
    // Default thickness 0.07 em of 24 px rounds to 2 rows.
    assert_eq!(runs(&rows).len(), 1, "one bar: {rows:?}");
    assert_eq!(rows.len(), 2, "two rows thick: {rows:?}");
    // It crosses the capitals inside their ink, and every pixel it changed
    // is the label's grey.
    let ink = ink_rows(&plain);
    assert!(rows[0] > ink[0] && rows[1] < *ink.last().unwrap());
    for x in 0..struck.width {
        let p = struck.pixel(x, rows[0]);
        if p != plain.pixel(x, rows[0]) {
            assert!(p[0] == p[1] && p[1] == p[2], "bar pixel not grey: {p:?}");
        }
    }

    let thick = render(
        &recipe(
            "body",
            r#", "strikethrough": true, "strikethrough-width": 4"#,
            false,
        ),
        feats(),
    );
    assert_eq!(changed_rows(&plain, &thick).len(), 4);
}

#[test]
fn strikethrough_expr_strikes_only_the_features_it_selects() {
    // An abolished number in the top half, a current one in the bottom half.
    let feats = || {
        vec![
            point("HH", true, 2048, 1024),
            point("LL", false, 2048, 3072),
        ]
    };
    let plain = render(&recipe("body", "", false), feats());
    let by_expr = render(
        &recipe(
            "body",
            r#", "strikethrough-expr": ["get", "abolished"]"#,
            false,
        ),
        feats(),
    );
    let all = render(
        &recipe("body", r#", "strikethrough": true"#, false),
        feats(),
    );
    let struck_rows = changed_rows(&plain, &by_expr);
    assert!(!struck_rows.is_empty(), "the abolished label is struck");
    assert!(
        struck_rows.iter().all(|&r| r < 32),
        "only the top label changes: {struck_rows:?}"
    );
    // The expression overrides the constant: with both set, the current
    // number stays plain.
    let both = render(
        &recipe(
            "body",
            r#", "strikethrough": true, "strikethrough-expr": ["get", "abolished"]"#,
            false,
        ),
        feats(),
    );
    assert_eq!(both.pixels, by_expr.pixels);
    assert!(changed_rows(&by_expr, &all).iter().all(|&r| r >= 32));
}

#[test]
fn a_wrapped_label_gets_a_bar_per_line() {
    let feats = || vec![point("HH HH", true, 2048, 2048)];
    let extra = r#", "max-width-em": 1"#;
    let plain = render(&recipe("body", extra, false), feats());
    let struck = render(
        &recipe(
            "body",
            r#", "max-width-em": 1, "strikethrough-expr": ["get", "abolished"]"#,
            false,
        ),
        feats(),
    );
    let bars = runs(&changed_rows(&plain, &struck));
    assert_eq!(bars.len(), 2, "one bar per line: {bars:?}");
    // One `line-height` (1.2 em = 28.8 px) apart.
    let gap = bars[1][0] as i32 - bars[0][0] as i32;
    assert!((gap - 29).abs() <= 1, "bars {bars:?}");
}

#[test]
fn a_glyphs_stack_strikes_through_its_digits() {
    // Glyph PBFs carry no strikeout metrics and lay out from the ascender
    // line; the bar must still cross the digits, not float above them.
    let feats = || vec![point("12", true, 2048, 2048)];
    let plain = render(&recipe("labels", "", false), feats());
    let struck = render(
        &recipe("labels", r#", "strikethrough": true"#, false),
        feats(),
    );
    let rows = changed_rows(&plain, &struck);
    assert_eq!(runs(&rows).len(), 1, "one bar: {rows:?}");
    let ink = ink_rows(&plain);
    let (top, bottom) = (ink[0] as f32, *ink.last().unwrap() as f32);
    for &r in &rows {
        let t = (r as f32 - top) / (bottom - top);
        assert!(
            (0.3..=0.8).contains(&t),
            "bar row {r} at {t:.2} of the ink {top}..{bottom}"
        );
    }
}

#[test]
fn text_draw_strikes_through_like_the_text_node() {
    let feats = || {
        vec![
            point("HH", true, 2048, 1024),
            point("LL", false, 2048, 3072),
        ]
    };
    let extra = r#", "strikethrough-expr": ["get", "abolished"], "halo-width": 2"#;
    let whole = render(&recipe("body", extra, false), feats());
    let shared = render(&recipe("body", extra, true), feats());
    let plain = render(&recipe("body", r#", "halo-width": 2"#, false), feats());
    assert_ne!(whole.pixels, plain.pixels, "the bar draws");
    assert_eq!(whole.pixels, shared.pixels);
}
