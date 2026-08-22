//! `stamp`'s `position-jitter-px`: a per-point offset in canvas pixels,
//! seeded from the point's world position so a stamp near a tile edge lands
//! in the same place whichever tile draws it.

use crate::common::{disk_sprite, render_with_features_and_images};
use ezu_features::{Feature, FeatureLayer, Geometry};
use ezu_graph::{RasterBuf, TileId};
use std::collections::HashMap;

/// One point feature at extent coords `(x, y)`.
fn point_layer(x: i32, y: i32) -> FeatureLayer {
    let mut geometry = Geometry::default();
    geometry.points.push((x, y));
    FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features: vec![Feature {
            id: None,
            geometry,
            properties: HashMap::new(),
        }],
    }
}

/// A recipe stamping a disk at every point, with `jitter` px of position
/// jitter — or none at all when `jitter` is `None`, so the default path and
/// an explicit `0` can be compared.
fn recipe(jitter: Option<f64>) -> String {
    let field = match jitter {
        Some(j) => format!(r#", "position-jitter-px": {j}"#),
        None => String::new(),
    };
    format!(
        r##"{{
      "name": "stamp-jitter",
      "tile-size": 64,
      "sources": {{
        "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "dot": {{ "type": "image", "src": "builtin:dot" }}
      }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "pts" }},
        "img":   {{ "op": "image", "src": "@dot" }},
        "out":   {{ "op": "stamp", "features": "@feats", "image": "@img"{field} }}
      }},
      "output": "@out"
    }}"##
    )
}

fn render(jitter: Option<f64>, tile: TileId, layer: FeatureLayer) -> std::sync::Arc<RasterBuf> {
    render_with_features_and_images(
        &recipe(jitter),
        64,
        32,
        tile,
        &[("src.pts", layer)],
        &[("dot", disk_sprite(8, 8, 3.0, [255, 0, 0, 255]))],
    )
}

/// Alpha-weighted centroid of the painted pixels, in canvas coords.
fn centroid(r: &RasterBuf) -> (f64, f64) {
    let (mut sx, mut sy, mut w) = (0.0, 0.0, 0.0);
    for y in 0..r.height {
        for x in 0..r.width {
            let a = r.pixel(x, y)[3] as f64;
            sx += x as f64 * a;
            sy += y as f64 * a;
            w += a;
        }
    }
    assert!(w > 0.0, "nothing was stamped");
    (sx / w, sy / w)
}

#[test]
fn zero_position_jitter_leaves_the_layout_untouched() {
    let tile = TileId { z: 0, x: 0, y: 0 };
    let plain = render(None, tile, point_layer(2048, 2048));
    let zero = render(Some(0.0), tile, point_layer(2048, 2048));
    assert_eq!(
        plain.pixels, zero.pixels,
        "`position-jitter-px: 0` should render identically to omitting it"
    );
}

#[test]
fn a_point_lands_in_the_same_world_place_from_either_tile() {
    // z1: the point sits just inside the right tile at extent x=256, and the
    // same world point reaches the left tile through the MVT buffer at
    // x=4352 — one full extent further right, i.e. 64 canvas px.
    let left = TileId { z: 1, x: 0, y: 0 };
    let right = TileId { z: 1, x: 1, y: 0 };
    let jitter = Some(12.0);

    let (lx, ly) = centroid(&render(jitter, left, point_layer(4352, 2048)));
    let (rx, ry) = centroid(&render(jitter, right, point_layer(256, 2048)));
    assert!(
        (lx - rx - 64.0).abs() < 1e-3 && (ly - ry).abs() < 1e-3,
        "the same point jumped between tiles: left ({lx}, {ly}), right ({rx}, {ry})"
    );

    // And the agreement is not the trivial one: the jitter did move the stamp
    // off the un-jittered position.
    let (ux, uy) = centroid(&render(None, right, point_layer(256, 2048)));
    assert!(
        (rx - ux).abs() > 0.5 || (ry - uy).abs() > 0.5,
        "jitter did not move the stamp: ({rx}, {ry}) vs un-jittered ({ux}, {uy})"
    );
}
