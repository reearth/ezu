//! Shared cross-layer label placement: `text-labels` + `label-placement` +
//! `text-draw`. MapLibre runs one collision index for every symbol layer, so
//! a POI label knocks out an overlapping road name; these tests exercise that
//! through the whole render path, plus the properties the per-layer path
//! already guaranteed — a lone layer placing exactly as the self-contained
//! `text` node does, and adjacent tiles agreeing on the seam.

use crate::common::render_with_features_and_images;
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

fn font_url() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    format!("file:{}", path.display()).replace('\\', "/")
}

/// A point feature at extent coords carrying its label in `name`.
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

/// A polyline feature at extent coords carrying its label in `name`.
fn line(name: &str, pts: &[(i32, i32)]) -> Feature {
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), Value::String(name.to_string()));
    let mut geometry = Geometry::default();
    geometry.lines.push(pts.to_vec());
    Feature {
        id: None,
        geometry,
        properties,
    }
}

fn layer(name: &str, features: Vec<Feature>) -> FeatureLayer {
    FeatureLayer {
        name: name.to_string(),
        extent: 4096,
        features,
    }
}

/// Pixels whose alpha reads as ink.
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

/// A two-label-layer recipe: `roads` (below) and `pois` (above), each with its
/// own `text-labels` node, both decided by one `label-placement` node and
/// drawn by their own `text-draw`. `roads_extra` / `pois_extra` inject
/// per-test fields; `output` names the node to render (a layer's own
/// `text-draw`, or the stack of both).
fn two_layer_recipe(roads_extra: &str, pois_extra: &str, output: &str) -> String {
    format!(
        r##"{{
      "name": "shared-placement",
      "tile-size": 64,
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "road_feats": {{ "op": "features", "source": "src", "layer": "roads" }},
        "poi_feats":  {{ "op": "features", "source": "src", "layer": "pois" }},
        "road_labels": {{ "op": "text-labels", "features": "@road_feats", "font": ["body"],
                          "text": ["get", "name"], "size": 12, "color": "#ff0000"
                          {roads_extra} }},
        "poi_labels":  {{ "op": "text-labels", "features": "@poi_feats", "font": ["body"],
                          "text": ["get", "name"], "size": 12, "color": "#0000ff"
                          {pois_extra} }},
        "placed": {{ "op": "label-placement", "labels": ["@road_labels", "@poi_labels"] }},
        "roads": {{ "op": "text-draw", "labels": "@road_labels", "placement": "@placed" }},
        "pois":  {{ "op": "text-draw", "labels": "@poi_labels",  "placement": "@placed" }},
        "both":  {{ "op": "stack", "layers": ["@roads", "@pois"] }}
      }},
      "output": "@{output}"
    }}"##,
        font = font_url(),
    )
}

/// Render one tile of a recipe with the given road / POI layers bound.
fn render(
    recipe: &str,
    tile: TileId,
    roads: Vec<Feature>,
    pois: Vec<Feature>,
) -> std::sync::Arc<ezu_graph::RasterBuf> {
    render_with_features_and_images(
        recipe,
        64,
        8,
        tile,
        &[
            ("src.roads", layer("roads", roads)),
            ("src.pois", layer("pois", pois)),
        ],
        &[],
    )
}

const T0: TileId = TileId { z: 0, x: 0, y: 0 };

#[test]
fn a_poi_label_knocks_out_an_overlapping_road_label() {
    // A road name and a POI label overlap at the tile centre. Sharing one
    // collision index, the POI layer (drawn on top, so placed first) keeps its
    // label and the road label drops — the whole point of the shared stage.
    let recipe = two_layer_recipe("", "", "roads");
    let road = || vec![point("MMMM", 1980, 2048)];
    let poi = || vec![point("WWWW", 2120, 2048)];

    let with_poi = render(&recipe, T0, road(), poi());
    let alone = render(&recipe, T0, road(), vec![]);
    assert!(ink(&alone) > 0, "the road label draws with no POI nearby");
    assert_eq!(
        ink(&with_poi),
        0,
        "the overlapping POI label must knock the road label out"
    );

    // The POI keeps its own label either way.
    let pois_out = two_layer_recipe("", "", "pois");
    assert!(ink(&render(&pois_out, T0, road(), poi())) > 0);
}

#[test]
fn priority_follows_layer_order_not_the_other_way_round() {
    // Reversing which layer sits on top reverses the winner: whichever layer
    // is last in `label-placement`'s list places first (maplibre-gl-js walks
    // symbol layers top-down). Same features, same styling — only the order
    // of the placement node's `labels` differs.
    let flipped = two_layer_recipe("", "", "roads").replace(
        r#""labels": ["@road_labels", "@poi_labels"]"#,
        r#""labels": ["@poi_labels", "@road_labels"]"#,
    );
    let ink_roads = ink(&render(
        &flipped,
        T0,
        vec![point("MMMM", 1980, 2048)],
        vec![point("WWWW", 2120, 2048)],
    ));
    assert!(
        ink_roads > 0,
        "with the road layer on top its label must win instead"
    );
}

#[test]
fn a_poi_label_knocks_out_an_overlapping_line_label() {
    // Point and line labels share the one index too: a POI label placed first
    // blocks a street name whose glyph boxes run through it. (Road labels are
    // line-placed in real basemaps, so this is the case that matters.)
    let recipe = two_layer_recipe(r#", "placement": "line", "spacing-px": 250"#, "", "roads");
    // The road ends inside the tile, so with no anchor fitting the spacing
    // grid it falls back to one at the middle of the run — where the POI is.
    let road = || vec![line("MMMM", &[(200, 2048), (3900, 2048)])];
    let poi = || vec![point("WWWW", 2048, 2048)];

    let alone = render(&recipe, T0, road(), vec![]);
    assert!(ink(&alone) > 0, "the street name draws with no POI nearby");
    let with_poi = render(&recipe, T0, road(), poi());
    assert!(
        ink(&with_poi) < ink(&alone),
        "the POI label must block the overlapping street name: {} vs {}",
        ink(&with_poi),
        ink(&alone)
    );
}

/// A lone label layer, either self-contained (`text`) or routed through the
/// shared stage. `op` picks which; the fields are otherwise identical.
fn one_layer_recipe(shared: bool) -> String {
    let nodes = if shared {
        r#""labels": { "op": "text-labels", "features": "@feats", "font": ["body"],
                       "text": ["get", "name"], "size": 12,
                       "source": "src", "layer": "roads" },
           "placed": { "op": "label-placement", "labels": ["@labels"] },
           "out":    { "op": "text-draw", "labels": "@labels", "placement": "@placed" }"#
    } else {
        r#""out": { "op": "text", "features": "@feats", "font": ["body"],
                    "text": ["get", "name"], "size": 12,
                    "source": "src", "layer": "roads" }"#
    };
    format!(
        r##"{{
      "name": "one-layer",
      "tile-size": 64,
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "roads" }},
        {nodes}
      }},
      "output": "@out"
    }}"##,
        font = font_url(),
    )
}

#[test]
fn one_label_layer_places_exactly_as_the_self_contained_node() {
    // A recipe with a single label layer must render identically whether it
    // places itself (`text`) or goes through the shared stage — the shared
    // placement is the same engine over one layer.
    let feats = || {
        vec![
            point("MMMM", 1980, 2048),
            point("WWWW", 2120, 2048), // overlaps the first
            point("II", 700, 900),
            point("LL", 3300, 3200),
        ]
    };
    let alone = render_with_features_and_images(
        &one_layer_recipe(false),
        64,
        8,
        T0,
        &[("src.roads", layer("roads", feats()))],
        &[],
    );
    let shared = render_with_features_and_images(
        &one_layer_recipe(true),
        64,
        8,
        T0,
        &[("src.roads", layer("roads", feats()))],
        &[],
    );
    assert!(ink(&alone) > 0, "expected label ink");
    assert_eq!(
        alone.pixels, shared.pixels,
        "a lone layer must be unchanged by the shared placement stage"
    );
}

#[test]
fn two_layer_seam_is_identical_across_adjacent_tiles() {
    // Two horizontally adjacent tiles see the same 3×3 window (each other's
    // layers bound as neighbours), and the labels straddling their shared edge
    // are decided by the same cross-layer index — so the shared border strip
    // must come out byte-for-byte identical. This is the seamlessness the
    // world-space design exists to guarantee, now across layers.
    let recipe = two_layer_recipe(
        r#", "source": "src", "layer": "roads""#,
        r#", "source": "src", "layer": "pois""#,
        "both",
    );
    let tile_size = 64u32;
    let pad = 8u32;
    // In A's frame the shared border is x = 4096; in B's frame it is x = 0.
    // The POI sits a little above the road label, close enough to compete.
    let roads_a = || vec![point("MMMM", 4096, 2048)];
    let roads_b = || vec![point("MMMM", 0, 2048)];
    let pois_a = || vec![point("WWWW", 4096, 2100)];
    let pois_b = || vec![point("WWWW", 0, 2100)];

    let left = render_with_features_and_images(
        &recipe,
        tile_size,
        pad,
        TileId { z: 4, x: 5, y: 7 },
        &[
            ("src.roads", layer("roads", roads_a())),
            ("src.pois", layer("pois", pois_a())),
            ("src.roads@1,0", layer("roads", roads_b())),
            ("src.pois@1,0", layer("pois", pois_b())),
        ],
        &[],
    );
    let right = render_with_features_and_images(
        &recipe,
        tile_size,
        pad,
        TileId { z: 4, x: 6, y: 7 },
        &[
            ("src.roads", layer("roads", roads_b())),
            ("src.pois", layer("pois", pois_b())),
            ("src.roads@-1,0", layer("roads", roads_a())),
            ("src.pois@-1,0", layer("pois", pois_a())),
        ],
        &[],
    );

    // A's right pad (world x ≥ 6·4096) against B's left interior (same world
    // x): identical, and carrying the winner's ink.
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

#[test]
fn distant_labels_do_not_interfere() {
    // Sharing the index must not make unrelated layers fight: a POI far from
    // the road label leaves it untouched.
    let recipe = two_layer_recipe("", "", "roads");
    let road = || vec![point("MM", 800, 800)];
    let alone = render(&recipe, T0, road(), vec![]);
    let with_far_poi = render(&recipe, T0, road(), vec![point("WW", 3600, 3600)]);
    assert_eq!(alone.pixels, with_far_poi.pixels);
}
