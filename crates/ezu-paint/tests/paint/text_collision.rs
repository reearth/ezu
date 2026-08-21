//! `text` node collision & dedup: the deterministic cross-tile placement
//! added in phase 2. The pure ordering/dedup/grid logic is unit-tested in
//! `ezu_core::text::collide`; these tests exercise the whole render path —
//! including the neighbour-binding seam that keeps tile borders seamless.

use crate::common::render_with_features_and_images;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

/// A point feature at extent coords with a `name` (the label) and a
/// numeric `rank` (a `sort-key-expr` source; lower ranks place first).
fn feat(name: &str, rank: i64, x: i32, y: i32) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), Value::String(name.to_string()));
    properties.insert("rank".to_string(), Value::Int(rank));
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

/// Recipe with the collision knobs threaded through. `extra` injects
/// per-test fields (allow-overlap, sort-key-expr, source/layer, …).
fn recipe(extra: &str) -> String {
    format!(
        r##"{{
      "name": "text-collision",
      "tile-size": 64,
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        "out":   {{ "op": "text", "features": "@feats", "font": ["body"],
                    "text": ["get", "name"], "size": 24 {extra} }}
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

fn render1(recipe: &str, feats: Vec<Feature>) -> std::sync::Arc<ezu_graph::RasterBuf> {
    render_with_features_and_images(
        recipe,
        64,
        8,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.pts", layer(feats))],
        &[],
    )
}

#[test]
fn overlapping_labels_collide_lower_sort_key_wins() {
    // Two heavily overlapping wide labels at the tile centre. With
    // collision on, only one survives; with allow-overlap both draw.
    let both = |extra: &str| {
        render1(
            &recipe(extra),
            vec![feat("MMMM", 1, 1980, 2048), feat("WWWW", 0, 2120, 2048)],
        )
    };
    let collided = both(r#", "sort-key-expr": ["get", "rank"]"#);
    let overlapped = both(r#", "sort-key-expr": ["get", "rank"], "allow-overlap": true"#);
    // Collision drops one label, so it inks strictly less than the
    // allow-overlap render that keeps both.
    assert!(
        ink(&collided) < ink(&overlapped),
        "collision should drop the loser: {} vs {}",
        ink(&collided),
        ink(&overlapped)
    );
    assert!(ink(&collided) > 0, "the winner must still draw");
}

#[test]
fn collide_false_draws_everything() {
    // The same overlapping pair with collision off keeps both — same
    // ink as allow-overlap.
    let feats = || vec![feat("MMMM", 1, 1980, 2048), feat("WWWW", 0, 2120, 2048)];
    let off = render1(&recipe(r#", "collide": false"#), feats());
    let overlap = render1(&recipe(r#", "allow-overlap": true"#), feats());
    assert_eq!(
        off.pixels, overlap.pixels,
        "collide:false must draw exactly what allow-overlap draws"
    );
}

#[test]
fn padding_px_inflates_the_collision_box() {
    // Two narrow labels with a clear gap: they don't collide at a small
    // padding, but a huge padding inflates their boxes into a collision.
    let feats = || vec![feat("II", 0, 1200, 2048), feat("LL", 1, 2900, 2048)];
    // (distinct text so they are not deduped)
    let small = render1(
        &recipe(r#", "sort-key-expr": ["get", "rank"], "padding-px": 1"#),
        feats(),
    );
    let big = render1(
        &recipe(r#", "sort-key-expr": ["get", "rank"], "padding-px": 400"#),
        feats(),
    );
    assert!(
        ink(&big) < ink(&small),
        "huge padding should force a collision the small padding avoids: {} vs {}",
        ink(&big),
        ink(&small)
    );
}

#[test]
fn missing_neighbour_bindings_degrade_to_centre_only() {
    // `source`/`layer` set (so the node requests neighbour bindings) but
    // none are bound — must render the centre label without error.
    let r = render1(
        &recipe(r#", "source": "src", "layer": "pts""#),
        vec![feat("HH", 0, 2048, 2048)],
    );
    assert!(ink(&r) > 0, "centre label still draws with no neighbours");
}

#[test]
fn duplicate_feature_across_tiles_renders_once() {
    // The same feature present in both the tile's own layer and a bound
    // neighbour (at the identical world position) must render exactly like
    // binding it only in the centre — deduped, no double-strength halo.
    let rc = recipe(r#", "source": "src", "layer": "pts", "halo-width": 2"#);
    let here = feat("HH", 0, 2048, 2048);

    let centre_only = render_with_features_and_images(
        &rc,
        64,
        8,
        TileId { z: 4, x: 5, y: 7 },
        &[("src.pts", layer(vec![here.clone()]))],
        &[],
    );
    // Bind the *same* feature again as the west neighbour, but placed in
    // that neighbour's frame (+one tile in x) so its world anchor matches.
    let dup_in_west = feat("HH", 0, 2048 + 4096, 2048);
    let with_dup = render_with_features_and_images(
        &rc,
        64,
        8,
        TileId { z: 4, x: 5, y: 7 },
        &[
            ("src.pts", layer(vec![here])),
            ("src.pts@-1,0", layer(vec![dup_in_west])),
        ],
        &[],
    );
    assert_eq!(
        centre_only.pixels, with_dup.pixels,
        "a feature bound twice at one world position must render once"
    );
}

#[test]
fn seam_is_identical_across_adjacent_tiles() {
    // Two horizontally adjacent tiles A=(5,7) and B=(6,7). A label P and a
    // competitor Q sit exactly on their shared world edge; P has the lower
    // sort-key. Both tiles see the same 3×3 window (each other's layer
    // bound as the neighbour), so both must pick P and draw it identically
    // in the shared border strip — the world-anchored seamlessness the
    // whole design exists to guarantee. (Mirrors noise_warp.rs's seam test.)
    let tile_size = 64u32;
    let pad = 8u32;
    let rc = recipe(r#", "source": "src", "layer": "pts", "sort-key-expr": ["get", "rank"]"#);

    // In A's frame the border is x = 4096; in B's frame it is x = 0.
    let layer_a = || layer(vec![feat("HH", 0, 4096, 2048), feat("WW", 1, 4096, 2048)]);
    let layer_b = || layer(vec![feat("HH", 0, 0, 2048), feat("WW", 1, 0, 2048)]);

    // A: own = layer_a, right neighbour (dx=+1) = layer_b.
    let left = render_with_features_and_images(
        &rc,
        tile_size,
        pad,
        TileId { z: 4, x: 5, y: 7 },
        &[("src.pts", layer_a()), ("src.pts@1,0", layer_b())],
        &[],
    );
    // B: own = layer_b, left neighbour (dx=-1) = layer_a.
    let right = render_with_features_and_images(
        &rc,
        tile_size,
        pad,
        TileId { z: 4, x: 6, y: 7 },
        &[("src.pts", layer_b()), ("src.pts@-1,0", layer_a())],
        &[],
    );

    // Compare the shared border columns: A's right pad (world x ≥ 6·4096)
    // against B's left interior (same world x). Byte-for-byte identical.
    let mut matched = 0usize;
    for y in 0..left.height {
        for dx in 0..pad {
            let l = left.pixel(tile_size + pad + dx, y);
            let r = right.pixel(pad + dx, y);
            assert_eq!(
                l, r,
                "seam mismatch at dx={dx}, y={y}: {l:?} vs {r:?} (tiles disagree on the winner)"
            );
            if l[3] > 100 {
                matched += 1;
            }
        }
    }
    assert!(
        matched > 0,
        "expected the winning label's ink in the shared border strip"
    );
}
