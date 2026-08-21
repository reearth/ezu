//! `text` node line placement (`symbol-placement: line` / `line-center`):
//! labels shaped once then walked along a polyline, rotated to the local
//! tangent, with per-glyph collision. The pure anchor/walk geometry is
//! unit-tested in `ezu_core::text::line`; these exercise the whole render
//! path — drawing along a diagonal, collision, and cross-tile dedup.

use crate::common::render_with_features_and_images;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

/// A line feature (one polyline) with a `name` label and numeric `rank`.
fn line_feature(name: &str, rank: i64, pts: &[(i32, i32)]) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), Value::String(name.to_string()));
    properties.insert("rank".to_string(), Value::Int(rank));
    let mut geometry = Geometry::default();
    geometry.lines.push(pts.to_vec());
    Feature {
        id: None,
        geometry,
        properties,
    }
}

fn layer(features: Vec<Feature>) -> FeatureLayer {
    FeatureLayer {
        name: "lines".to_string(),
        extent: 4096,
        features,
    }
}

/// Recipe with a line-placed `text` node; `extra` injects per-test fields.
fn recipe(extra: &str) -> String {
    format!(
        r##"{{
      "name": "text-line",
      "tile-size": 64,
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "lines" }},
        "out":   {{ "op": "text", "features": "@feats", "font": ["body"],
                    "text": ["get", "name"], "size": 16, "placement": "line-center" {extra} }}
      }},
      "output": "@out"
    }}"##,
        font = font_url(),
        extra = extra,
    )
}

fn ink(r: &ezu_graph::RasterBuf) -> usize {
    let mut n = 0;
    for y in 0..r.height {
        for x in 0..r.width {
            if r.pixel(x, y)[3] > 100 {
                n += 1;
            }
        }
    }
    n
}

fn ink_in(r: &ezu_graph::RasterBuf, x0: u32, y0: u32, x1: u32, y1: u32) -> usize {
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            if r.pixel(x, y)[3] > 100 {
                n += 1;
            }
        }
    }
    n
}

fn render1(recipe: &str, feats: Vec<Feature>) -> std::sync::Arc<ezu_graph::RasterBuf> {
    render_with_features_and_images(
        recipe,
        64,
        8,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.lines", layer(feats))],
        &[],
    )
}

#[test]
fn label_renders_along_a_diagonal_line() {
    // A diagonal line corner-to-corner; the centred label's glyphs follow
    // it, so ink lands in the central diagonal band and the off-diagonal
    // (top-right) corner stays empty.
    let r = render1(
        &recipe(""),
        vec![line_feature("MMMM", 0, &[(0, 0), (4096, 4096)])],
    );
    // Tile is 64 px with an 8 px pad → the tile body is device [8, 72).
    let centre = ink_in(&r, 24, 24, 56, 56);
    assert!(
        centre > 20,
        "expected label ink along the diagonal: {centre}"
    );
    // The anti-diagonal (top-right) corner of the tile body is far from
    // the path — no ink there.
    let corner = ink_in(&r, 56, 8, 72, 24);
    assert_eq!(corner, 0, "no ink off the diagonal path: {corner}");
}

#[test]
fn line_labels_collide_lower_sort_key_wins() {
    // Two overlapping horizontal lines through the tile centre, distinct
    // text so they are not deduped. With collision the lower-rank label
    // wins and the other drops; allow-overlap keeps both.
    let feats = || {
        vec![
            line_feature("AAAA", 0, &[(512, 2000), (3584, 2000)]),
            line_feature("BBBB", 1, &[(512, 2096), (3584, 2096)]),
        ]
    };
    let collided = render1(&recipe(r#", "sort-key-expr": ["get", "rank"]"#), feats());
    let overlapped = render1(
        &recipe(r#", "sort-key-expr": ["get", "rank"], "allow-overlap": true"#),
        feats(),
    );
    assert!(
        ink(&collided) < ink(&overlapped),
        "collision should drop the loser: {} vs {}",
        ink(&collided),
        ink(&overlapped)
    );
    assert!(ink(&collided) > 0, "the winner must still draw");
}

#[test]
fn collide_false_draws_every_line_label() {
    // The same overlapping pair with collision off keeps both — identical
    // to allow-overlap.
    let feats = || {
        vec![
            line_feature("AAAA", 0, &[(512, 2000), (3584, 2000)]),
            line_feature("BBBB", 1, &[(512, 2096), (3584, 2096)]),
        ]
    };
    let off = render1(&recipe(r#", "collide": false"#), feats());
    let overlap = render1(&recipe(r#", "allow-overlap": true"#), feats());
    assert_eq!(
        off.pixels, overlap.pixels,
        "collide:false must draw exactly what allow-overlap draws"
    );
}

#[test]
fn duplicate_line_across_tiles_renders_once() {
    // A line-center label whose midpoint sits at a fixed world position,
    // present both in the tile's own layer and (shifted into that frame)
    // as the west neighbour. Deduped: it must render exactly like binding
    // it only in the centre. This is the cross-tile determinism guarantee
    // for lines — MVT clips a line differently per tile, but a label whose
    // window lies inside the shared strip yields the same world anchor and
    // is deduped by (text, quantized anchor).
    let rc = recipe(r#", "source": "src", "layer": "lines", "halo-width": 2"#);
    // Midpoint at (2048, 2048) in tile (5, 7).
    let here = line_feature("HERE", 0, &[(1024, 2048), (3072, 2048)]);

    let centre_only = render_with_features_and_images(
        &rc,
        64,
        8,
        TileId { z: 4, x: 5, y: 7 },
        &[("src.lines", layer(vec![here.clone()]))],
        &[],
    );
    // The same line in the west neighbour's frame (+1 tile in x) so its
    // midpoint maps to the identical world anchor.
    let dup_in_west = line_feature("HERE", 0, &[(1024 + 4096, 2048), (3072 + 4096, 2048)]);
    let with_dup = render_with_features_and_images(
        &rc,
        64,
        8,
        TileId { z: 4, x: 5, y: 7 },
        &[
            ("src.lines", layer(vec![here])),
            ("src.lines@-1,0", layer(vec![dup_in_west])),
        ],
        &[],
    );
    assert_eq!(
        centre_only.pixels, with_dup.pixels,
        "a line label bound twice at one world position must render once"
    );
}
