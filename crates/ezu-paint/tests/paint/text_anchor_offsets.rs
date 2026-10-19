//! `text` / `text-labels` `anchor-offsets`: the positions a point label
//! tries, each an anchor plus its own shift. Read through `label-boxes`, which
//! reports the position taken as `variant-index`, and checked against the
//! older `anchor-variants` / `radial-offset` fields it replaces.

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

/// 16 px labels through `text-labels` → `label-placement`, drawn by
/// `text-draw` over their `label-boxes`. `extra` adds fields to the label
/// layer.
fn recipe(extra: &str) -> String {
    format!(
        r##"{{
      "name": "anchor-offsets",
      "tile-size": {TILE},
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats":  {{ "op": "features", "source": "src", "layer": "pts" }},
        "labels": {{ "op": "text-labels", "features": "@feats", "source": "src", "layer": "pts",
                     "font": ["body"], "text": ["get", "name"], "size": 16,
                     "color": "#ffffff" {extra} }},
        "placed": {{ "op": "label-placement", "labels": ["@labels"] }},
        "boxes":  {{ "op": "label-boxes", "placement": "@placed", "parts": ["text"] }},
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

/// The build-time warnings of a style, or its build error.
fn warnings(recipe: &str) -> Result<Vec<String>, String> {
    let doc = Document::from_json(recipe).map_err(|e| e.to_string())?;
    build_graph(&doc, &default_registry())
        .map(|g| g.warnings().to_vec())
        .map_err(|e| e.to_string())
}

/// One placed label's `(variant-index, overlapped, text box bounds in px)`.
type Placed = (f64, bool, (f32, f32, f32, f32));

/// Each placed label, in `variant-index` order.
fn placed(feats: &FilteredFeatures) -> Vec<Placed> {
    let mut v: Vec<_> = feats
        .groups
        .iter()
        .flat_map(|g| {
            let ix = match g.properties.get("variant-index") {
                Some(maplibre_expr::Value::Number(n)) => *n,
                other => panic!("variant-index: {other:?}"),
            };
            let overlapped = match g.properties.get("overlapped") {
                Some(maplibre_expr::Value::Bool(b)) => *b,
                other => panic!("overlapped: {other:?}"),
            };
            g.polygons.iter().map(move |p| {
                let b = p.exterior.iter().fold(
                    (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
                    |(x0, y0, x1, y1), &(x, y)| {
                        let (x, y) = (x as f32 / UNITS_PER_PX, y as f32 / UNITS_PER_PX);
                        (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
                    },
                );
                (ix, overlapped, b)
            })
        })
        .collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v
}

#[test]
fn a_farther_position_takes_over_when_the_nearer_ones_collide() {
    // Three labels on one point, each position the same `bottom` anchor
    // 2 em (32 px) further up: the first label takes the nearest, the next
    // the one above it, the last the farthest.
    let feats = vec![
        point("AA", 2048, 3072),
        point("BB", 2048, 3072),
        point("CC", 2048, 3072),
    ];
    let (boxes, _) = run(
        &recipe(
            r#", "anchor-offsets": [
                 { "anchor": "bottom", "offset-em": [0, -0.5] },
                 { "anchor": "bottom", "offset-em": [0, -2.5] },
                 { "anchor": "bottom", "offset-em": [0, -4.5] }
               ]"#,
        ),
        feats,
    );
    let got = placed(&boxes);
    let ix: Vec<f64> = got.iter().map(|p| p.0).collect();
    assert_eq!(ix, [0.0, 1.0, 2.0], "{got:?}");
    assert!(got.iter().all(|p| !p.1), "no label overlaps: {got:?}");
    // Each block's bottom edge sits 32 px above the previous one's.
    for w in got.windows(2) {
        let step = (w[0].2).3 - (w[1].2).3;
        assert!((step - 32.0).abs() < 0.1, "step {step}: {got:?}");
    }
}

#[test]
fn radial_em_places_like_radial_offset() {
    // The deprecated pair and its `anchor-offsets` spelling render the same
    // pixels and the same boxes.
    let feats = || vec![point("AA", 2048, 2048), point("BB", 2048, 2048)];
    let old = run(
        &recipe(r#", "anchor-variants": ["top", "bottom-left"], "radial-offset": 1.0"#),
        feats(),
    );
    let new = run(
        &recipe(
            r#", "anchor-offsets": [
                 { "anchor": "top", "radial-em": 1.0 },
                 { "anchor": "bottom-left", "radial-em": 1.0 }
               ]"#,
        ),
        feats(),
    );
    assert_eq!(placed(&old.0), placed(&new.0));
    assert_eq!(placed(&new.0).len(), 2, "both labels place");
    assert!(old.1.pixels == new.1.pixels, "the renders differ");
}

#[test]
fn offset_em_shifts_like_the_plain_offset() {
    // A one-entry `anchor-offsets` with `offset-em` is the plain
    // `anchor` + `offset-em` pair: right and down positive, as written.
    let feats = || vec![point("AA", 2048, 2048)];
    let plain = run(
        &recipe(r#", "anchor": "top-left", "offset-em": [0.5, 0.25]"#),
        feats(),
    );
    let listed = run(
        &recipe(r#", "anchor-offsets": [{ "anchor": "top-left", "offset-em": [0.5, 0.25] }]"#),
        feats(),
    );
    let got = placed(&listed.0);
    assert_eq!(placed(&plain.0), got);
    assert!(plain.1.pixels == listed.1.pixels, "the renders differ");
    // `top-left` on (64, 64), shifted right by 8 px and down by 4 px.
    let b = got[0].2;
    assert!(b.0 >= 72.0 - 0.1 && b.1 >= 68.0 - 0.1, "{b:?}");
}

#[test]
fn the_deprecation_warning_spells_out_an_equivalent_replacement() {
    // `anchor-variants` with a plain `offset-em` mirrors the offset per
    // anchor; the warning's suggested `anchor-offsets` must place exactly
    // as the old fields do.
    let old = r#", "anchor-variants": ["top", "left"], "offset-em": [0.5, 0.75]"#;
    let w = warnings(&recipe(old)).expect("builds");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("`anchor-variants` is deprecated"), "{}", w[0]);
    let replacement = w[0]
        .split_once("write ")
        .and_then(|(_, r)| r.strip_suffix(" instead"))
        .unwrap_or_else(|| panic!("no replacement in {}", w[0]));
    assert!(
        replacement.starts_with("\"anchor-offsets\": ["),
        "{replacement}"
    );
    let new = format!(", {replacement}");
    assert!(warnings(&recipe(&new)).expect("builds").is_empty());

    let feats = || vec![point("AA", 2048, 2048), point("BB", 2048, 2048)];
    let (a, b) = (run(&recipe(old), feats()), run(&recipe(&new), feats()));
    assert_eq!(placed(&a.0), placed(&b.0));
    assert!(a.1.pixels == b.1.pixels, "the renders differ");
}

#[test]
fn radial_offset_names_its_replacement() {
    let w = warnings(&recipe(
        r#", "anchor-variants": ["top", "bottom"], "radial-offset": 1.5"#,
    ))
    .expect("builds");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].contains("`anchor-variants` and `radial-offset` are deprecated")
            && w[0].contains(r#"{ "anchor": "top", "radial-em": 1.5 }"#)
            && w[0].contains(r#"{ "anchor": "bottom", "radial-em": 1.5 }"#),
        "{}",
        w[0]
    );
}

#[test]
fn anchor_offsets_overrides_and_names_the_fields_it_ignores() {
    let w = warnings(&recipe(
        r#", "anchor": "top", "radial-offset": 2,
             "anchor-offsets": [{ "anchor": "left" }]"#,
    ))
    .expect("builds");
    assert_eq!(w.len(), 1, "no deprecation warning on top: {w:?}");
    assert!(
        w[0].contains("`anchor`, `radial-offset` ignored"),
        "{}",
        w[0]
    );
    // The plain single-position pair is not deprecated.
    let plain = warnings(&recipe(r#", "anchor": "top", "offset-em": [0, 1]"#)).expect("builds");
    assert!(plain.is_empty(), "{plain:?}");
}

#[test]
fn malformed_entries_fail_the_build() {
    for (bad, why) in [
        (r#"[]"#, "at least one entry"),
        (r#"[{ "anchor": "middle" }]"#, "unknown anchor"),
        (r#"[{ "offset-em": [0, 1] }]"#, "missing `anchor`"),
        (
            r#"[{ "anchor": "top", "offset-em": [0, 1], "radial-em": 1 }]"#,
            "not both",
        ),
        (
            r#"[{ "anchor": "top", "radial": 1 }]"#,
            "unknown key `radial`",
        ),
        (r#"[{ "anchor": "top", "offset-em": [1] }]"#, "[x, y]"),
    ] {
        let err = warnings(&recipe(&format!(r#", "anchor-offsets": {bad}"#))).expect_err(bad);
        assert!(err.contains(why), "{bad}: {err}");
    }
}
