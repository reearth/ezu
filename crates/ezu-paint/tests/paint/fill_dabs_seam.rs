//! `fill-dabs` anchors its candidate lattice in world space, so two tiles
//! meeting at a border agree on where every cell is and on how it jittered.
//!
//! Anchoring the lattice on the canvas instead — counting cells from the
//! tile's own corner — lines up only when `spacing-px` divides the tile
//! width, and steps the pattern by a fraction of a cell at every other
//! border. The pitches here deliberately divide neither.

use crate::common::render_with_features;
use ezu_features::{Feature, FeatureLayer, Geometry, Polygon};
use ezu_graph::TileId;
use std::collections::HashMap;

const SIZE: u32 = 128;
const PAD: u32 = 24;
const EXTENT: u32 = 4096;

/// A polygon covering the tile and well past its padding, so the fill mask
/// is solid across the whole padded canvas of either tile.
fn covering_layer() -> FeatureLayer {
    let e = EXTENT as i32;
    let mut geometry = Geometry::default();
    geometry.polygons.push(Polygon {
        exterior: vec![(-e, -e), (2 * e, -e), (2 * e, 2 * e), (-e, 2 * e), (-e, -e)],
        holes: vec![],
    });
    FeatureLayer {
        name: "poly".to_string(),
        extent: EXTENT,
        features: vec![Feature {
            id: None,
            geometry,
            properties: HashMap::new(),
        }],
    }
}

fn style(spacing_px: f64) -> String {
    format!(
        r##"{{
      "name": "dab-seam",
      "tile-size": {SIZE},
      "sources": {{ "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }} }},
      "nodes": {{
        "bg":   {{ "op": "solid", "color": "#ffffff" }},
        "feat": {{ "op": "features", "source": "src", "layer": "poly" }},
        "dabs": {{ "op": "fill-dabs", "features": "@feat",
                   "color": "#204080", "opacity": 0.35, "radius-px": 5,
                   "spacing-px": {spacing_px},
                   "position-jitter": 0.9, "size-jitter": 0.35,
                   "opacity-jitter": 0.25, "value-jitter": 0.08 }},
        "out":  {{ "op": "blend", "base": "@bg", "over": "@dabs" }}
      }},
      "output": "@out"
    }}"##
    )
}

fn render(spacing_px: f64, tile: TileId) -> std::sync::Arc<ezu_graph::RasterBuf> {
    let r = render_with_features(
        &style(spacing_px),
        SIZE,
        PAD,
        tile,
        &[("src.poly", covering_layer())],
    );
    assert_eq!(r.width, SIZE + 2 * PAD, "expected a padded buffer");
    r
}

/// Half-width of the strip compared either side of the shared edge. Kept
/// well inside both padded canvases so that every dab reaching the strip has
/// its centre on both — a dab is culled by its centre, so a wider strip
/// would compare pixels one tile draws and the other never sees.
const STRIP: u32 = 8;

/// Both tiles paint the band around their shared edge, and they have to
/// paint it identically: same cells, same jitter, same dabs.
fn seam_strip_matches(spacing_px: f64, z: u8, x: u32, y: u32) {
    let left = render(spacing_px, TileId { z, x, y });
    let right = render(spacing_px, TileId { z, x: x + 1, y });

    // The shared edge is the left tile's right border: padded column
    // `PAD + SIZE` there, and padded column `PAD` on the right tile.
    let mut compared = 0usize;
    let mut painted = 0usize;
    for row in PAD..(PAD + SIZE) {
        for d in 0..(2 * STRIP) {
            let lx = PAD + SIZE - STRIP + d;
            let rx = PAD - STRIP + d;
            let l = left.pixel(lx, row);
            let r = right.pixel(rx, row);
            assert_eq!(
                l, r,
                "seam mismatch at row {row}, offset {d} from the edge \
                 (left px {lx} = {l:?}, right px {rx} = {r:?}); \
                 the lattice moved between tiles"
            );
            compared += 1;
            if l != [255, 255, 255, 255] {
                painted += 1;
            }
        }
    }
    assert!(compared > 0);
    // A blank strip would satisfy the equality above without testing anything.
    assert!(
        painted * 4 > compared,
        "only {painted} of {compared} strip pixels carry paint; the fill did \
         not reach the seam, so the comparison proves nothing"
    );
}

#[test]
fn seam_is_continuous_for_a_pitch_that_divides_the_tile() {
    // 128 / 8 = 16 cells: the case a canvas-anchored lattice also gets right.
    seam_strip_matches(8.0, 13, 4000, 3000);
}

#[test]
fn seam_is_continuous_for_a_pitch_that_does_not_divide_the_tile() {
    // 128 / 7 is not whole, so a canvas-anchored lattice would step by
    // 128 mod 7 = 1 px at this border.
    seam_strip_matches(7.0, 13, 4000, 3000);
}

#[test]
fn seam_is_continuous_for_a_fractional_pitch() {
    seam_strip_matches(5.5, 13, 4000, 3000);
}

/// The lattice is global, so it also has to join tiles that are not
/// neighbours of the one above — including across a power-of-two boundary in
/// the tile index, where the global pixel origin carries into a higher bit.
#[test]
fn seam_is_continuous_across_a_power_of_two_tile_index() {
    seam_strip_matches(7.0, 13, 4095, 2048);
}
