//! `features` with `stitch`: the layer joined to its neighbouring tiles.
//! The cell ownership and the union are unit-tested next to the node;
//! these tests go through the whole build and render path — which names
//! the graph asks the host for, and what a stitched layer draws.

use crate::common::render_with_features;
use ezu_features::{Feature, FeatureLayer, Geometry, Polygon};
use ezu_graph::{build_graph, TileId};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;
use std::collections::HashMap;

const E: i32 = 4096;
/// The protomaps tile buffer, in extent units.
const BUF: i32 = 128;
const TILE: TileId = TileId { z: 4, x: 5, y: 7 };

fn recipe(stitch: Option<&str>) -> String {
    let stitch = stitch.map_or(String::new(), |s| format!(r#", "stitch": "{s}""#));
    format!(
        r##"{{
      "name": "stitch",
      "tile-size": 64,
      "sources": {{
        "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "water" {stitch} }},
        "out":   {{ "op": "fill-solid", "features": "@feats", "fill": "#00000080" }}
      }},
      "output": "@out"
    }}"##
    )
}

fn square(x0: i32, y0: i32, x1: i32, y1: i32) -> Feature {
    let mut geometry = Geometry::default();
    geometry.polygons.push(Polygon {
        exterior: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)],
        holes: vec![],
    });
    Feature {
        id: None,
        geometry,
        properties: HashMap::new(),
    }
}

fn layer(features: Vec<Feature>) -> FeatureLayer {
    FeatureLayer {
        name: "water".to_string(),
        extent: E as u32,
        features,
    }
}

/// One body of water running east out of the tile, as the centre and the
/// east neighbour each cut it: both copies reach `BUF` past their edge.
fn centre() -> FeatureLayer {
    layer(vec![square(2000, 0, E + BUF, E)])
}
fn east() -> FeatureLayer {
    layer(vec![square(-BUF, 0, 1000, E)])
}

fn render(recipe: &str, layers: &[(&str, FeatureLayer)]) -> std::sync::Arc<ezu_graph::RasterBuf> {
    render_with_features(recipe, 64, 8, TILE, layers)
}

#[test]
fn neighbour_names_are_requested_only_when_stitching() {
    let inputs = |stitch| {
        let doc = Document::from_json(&recipe(stitch)).expect("parse");
        build_graph(&doc, &default_registry())
            .expect("build")
            .asset_inputs()
    };
    let off = inputs(None);
    assert_eq!(off.into_iter().collect::<Vec<_>>(), vec!["src.water"]);
    for mode in ["merge", "pieces"] {
        let on = inputs(Some(mode));
        assert_eq!(on.len(), 9, "{mode}: {on:?}");
        assert!(on.contains("src.water"));
        for name in ezu_graph::neighbor_bindings("src.water") {
            assert!(on.contains(&name), "{mode}: missing {name}");
        }
    }
}

#[test]
fn an_unknown_mode_is_rejected() {
    let doc = Document::from_json(&recipe(Some("glue"))).expect("parse");
    let Err(err) = build_graph(&doc, &default_registry()) else {
        panic!("an unknown stitch mode must not build");
    };
    let err = err.to_string();
    assert!(err.contains("stitch"), "{err}");
}

#[test]
fn without_stitch_the_neighbours_are_ignored() {
    let alone = render(&recipe(None), &[("src.water", centre())]);
    let beside = render(
        &recipe(None),
        &[("src.water", centre()), ("src.water@1,0", east())],
    );
    assert_eq!(alone.pixels, beside.pixels);
}

#[test]
fn with_no_neighbour_bound_stitching_changes_nothing() {
    let plain = render(&recipe(None), &[("src.water", centre())]);
    for mode in ["merge", "pieces"] {
        let stitched = render(&recipe(Some(mode)), &[("src.water", centre())]);
        assert_eq!(plain.pixels, stitched.pixels, "{mode}");
    }
}

#[test]
fn stitched_overlap_is_filled_once() {
    // Padded canvas: 8 px pad, 64 px tile → the tile's east edge is at
    // x = 72, and the buffer both copies share covers the next 2 px.
    let alpha = |r: &ezu_graph::RasterBuf, x: u32| r.pixel(x, 40)[3];
    for mode in ["merge", "pieces"] {
        let r = render(
            &recipe(Some(mode)),
            &[("src.water", centre()), ("src.water@1,0", east())],
        );
        let inside = alpha(&r, 60);
        assert!(inside > 0, "{mode}: the water must draw");
        for x in 70..76 {
            assert_eq!(
                alpha(&r, x),
                inside,
                "{mode}: x = {x} drawn twice or not at all"
            );
        }
    }
}

#[test]
fn a_neighbour_carries_what_the_centre_buffer_lacks() {
    // Water only in the east neighbour, starting past the centre's
    // buffer: unstitched, the pad strip stays empty; stitched, it fills.
    let lake = layer(vec![square(2 * BUF, 0, 1000, E)]);
    let layers = [("src.water", layer(vec![])), ("src.water@1,0", lake)];
    let x = 78; // 6 px past the east edge, beyond the 2 px buffer
    let plain = render(&recipe(None), &layers);
    let stitched = render(&recipe(Some("pieces")), &layers);
    assert_eq!(plain.pixel(x, 40)[3], 0);
    assert!(stitched.pixel(x, 40)[3] > 0);
}
