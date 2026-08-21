//! `dot-density`: dots confined to their feature, counts that follow the
//! declared density, and the two properties that come from anchoring the
//! lattice in world space — placement that depends on the tile's world
//! position, and a count that tracks Mercator's latitude scale.

use crate::common::render_with_features;
use ezu_features::{Feature, FeatureLayer, Geometry, Polygon, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

const TILE: u32 = 128;
const EXTENT: i32 = 4096;

/// A rectangular polygon `[x0, x1] × [y0, y1]` in tile-local extent coords.
fn rect(x0: i32, y0: i32, x1: i32, y1: i32) -> Polygon {
    Polygon {
        exterior: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)],
        holes: vec![],
    }
}

fn layer_of(polys: Vec<Polygon>, pop_per_km2: f64) -> FeatureLayer {
    let mut properties = HashMap::new();
    properties.insert("d".to_string(), Value::Double(pop_per_km2));
    let mut geometry = Geometry::default();
    geometry.polygons.extend(polys);
    FeatureLayer {
        name: "poly".to_string(),
        extent: EXTENT as u32,
        features: vec![Feature {
            id: None,
            geometry,
            properties,
        }],
    }
}

/// Density (units per km², with `dot-value` 1) that fills roughly 40% of
/// the lattice cells at zoom `z` for the tile size and spacing below.
///
/// One extent unit spans `C / (extent · 2^z)` metres at the equator, so a
/// square unit is `(9.784 / 2^z)²` km² and a cell of `spacing²` units is
/// that again times `spacing²`. Real dot density maps land on numbers
/// this small because the areas involved are continental.
fn density_for(z: u8) -> f64 {
    let km_per_unit = 40_075.016_685_578_5 / (EXTENT as f64) / (1u64 << z) as f64;
    let spacing_units = 6.0 * EXTENT as f64 / TILE as f64;
    0.4 / (km_per_unit * km_per_unit * spacing_units * spacing_units)
}

fn style(z: u8) -> String {
    format!(
        r##"{{
      "name": "dots",
      "tile-size": {TILE},
      "sources": {{ "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }} }},
      "nodes": {{
        "bg":    {{ "op": "solid", "color": "#ffffff" }},
        "feats": {{ "op": "features", "source": "src", "layer": "poly" }},
        "dots":  {{ "op": "dot-density", "features": "@feats",
                   "density-expr": ["*", ["get", "d"], {}],
                   "spacing-px": 6 }},
        "paint": {{ "op": "circles", "features": "@dots", "radius": 1.0, "color": "#000000" }},
        "out":   {{ "op": "blend", "base": "@bg", "over": "@paint" }}
      }},
      "output": "@out"
    }}"##,
        density_for(z)
    )
}

/// Count pixels dark enough to be part of a dot, within a pixel box.
fn count_dots(r: &ezu_graph::RasterBuf, x0: u32, y0: u32, x1: u32, y1: u32) -> usize {
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            let p = r.pixel(x, y);
            if p[0] < 128 && p[1] < 128 && p[2] < 128 {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn dots_stay_inside_the_feature() {
    // Left half of the tile only.
    let layer = layer_of(vec![rect(0, 0, EXTENT / 2, EXTENT - 1)], 1.0);
    let r = render_with_features(
        &style(0),
        TILE,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.poly", layer)],
    );
    let inside = count_dots(&r, 2, 2, TILE / 2 - 4, TILE - 2);
    let outside = count_dots(&r, TILE / 2 + 4, 2, TILE - 2, TILE - 2);
    assert!(inside > 20, "expected dots in the polygon, got {inside}");
    assert_eq!(outside, 0, "dots escaped the polygon");
}

#[test]
fn holes_stay_empty() {
    let poly = Polygon {
        exterior: rect(0, 0, EXTENT - 1, EXTENT - 1).exterior,
        holes: vec![rect(EXTENT / 4, EXTENT / 4, EXTENT * 3 / 4, EXTENT * 3 / 4).exterior],
    };
    let r = render_with_features(
        &style(0),
        TILE,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.poly", layer_of(vec![poly], 1.0))],
    );
    // The hole covers the middle half of the tile; leave a margin for
    // dots that legitimately sit just outside its edge.
    let in_hole = count_dots(
        &r,
        TILE / 4 + 3,
        TILE / 4 + 3,
        TILE * 3 / 4 - 3,
        TILE * 3 / 4 - 3,
    );
    assert_eq!(in_hole, 0, "dots landed in the hole");
    let outer = count_dots(&r, 2, 2, TILE / 4 - 3, TILE - 2);
    assert!(outer > 10, "expected dots outside the hole, got {outer}");
}

#[test]
fn denser_features_get_more_dots() {
    let poly = rect(0, 0, EXTENT - 1, EXTENT - 1);
    let count = |multiplier: f64| {
        let r = render_with_features(
            &style(0),
            TILE,
            0,
            TileId { z: 0, x: 0, y: 0 },
            &[("src.poly", layer_of(vec![poly.clone()], multiplier))],
        );
        count_dots(&r, 0, 0, TILE, TILE)
    };
    // A quarter of the density should visibly thin the scatter out.
    let sparse = count(0.25);
    let dense = count(1.0);
    assert!(sparse > 0, "sparse render drew nothing");
    assert!(
        dense > sparse * 2,
        "density did not drive the count: {dense} vs {sparse}"
    );
}

#[test]
fn dot_value_thins_the_scatter() {
    let poly = rect(0, 0, EXTENT - 1, EXTENT - 1);
    let with_dot_value = |dot_value: f64| {
        let json = style(0).replace(
            "\"spacing-px\": 6",
            &format!("\"spacing-px\": 6, \"dot-value\": {dot_value}"),
        );
        let r = render_with_features(
            &json,
            TILE,
            0,
            TileId { z: 0, x: 0, y: 0 },
            &[("src.poly", layer_of(vec![poly.clone()], 1.0))],
        );
        count_dots(&r, 0, 0, TILE, TILE)
    };
    // One dot per 4 units instead of per unit: a quarter of the dots.
    let coarse = with_dot_value(4.0);
    let fine = with_dot_value(1.0);
    assert!(coarse > 0, "coarse render drew nothing");
    assert!(fine > coarse * 2, "dot-value ignored: {fine} vs {coarse}");
}

/// The lattice lives in world space, so the same local polygon on two
/// different tiles is cut by the grid differently. A tile-anchored
/// lattice would produce identical output — which is exactly the
/// behaviour that breaks at a seam.
#[test]
fn placement_follows_world_position() {
    let poly = rect(0, 0, EXTENT - 1, EXTENT - 1);
    let render_at = |x: u32| {
        render_with_features(
            &style(1),
            TILE,
            0,
            TileId { z: 1, x, y: 0 },
            &[("src.poly", layer_of(vec![poly.clone()], 1.0))],
        )
    };
    let a = render_at(0);
    let b = render_at(1);
    let differing = (0..TILE * TILE)
        .filter(|i| {
            let (x, y) = (i % TILE, i / TILE);
            a.pixel(x, y) != b.pixel(x, y)
        })
        .count();
    assert!(
        differing > 50,
        "neighbouring tiles drew the same dots ({differing} pixels differ)"
    );
}

/// Web Mercator stretches high latitudes, so a tile near the pole covers
/// far less ground than one at the equator. At a fixed density per km²
/// the polar tile must therefore hold fewer dots.
#[test]
fn count_follows_mercator_latitude_scale() {
    let poly = rect(0, 0, EXTENT - 1, EXTENT - 1);
    let count_at = |y: u32| {
        let r = render_with_features(
            &style(2),
            TILE,
            0,
            TileId { z: 2, x: 0, y },
            &[("src.poly", layer_of(vec![poly.clone()], 1.0))],
        );
        count_dots(&r, 0, 0, TILE, TILE)
    };
    let polar = count_at(0); // 85°N–66°N
    let equatorial = count_at(1); // 66°N–0°
    assert!(
        equatorial > polar * 2,
        "latitude scale not applied: {equatorial} near the equator vs {polar} near the pole"
    );
}

/// The absolute check the relative ones can't make: a density stated per
/// square kilometre has to mean a square kilometre. Everything here is
/// computed from the projection rather than recorded from a run, so a
/// mistake in the unit chain — a dropped `1000²`, a world span off by a
/// zoom level — shows up as a count that is orders of magnitude wrong.
#[test]
fn dot_count_matches_the_ground_area_it_covers() {
    // A tile at the equator, where Mercator's scale is 1 and one extent
    // unit is a flat `C / (extent · 2^z)` metres.
    const Z: u8 = 10;
    const SIZE: u32 = 512;
    const SPACING_PX: f64 = 16.0;
    const RADIUS_PX: f64 = 2.5;

    let m_per_unit = 40_075_016.685_578_5 / (EXTENT as f64) / (1u64 << Z) as f64;
    let km2_per_unit2 = (m_per_unit / 1000.0).powi(2);
    let tile_km2 = (EXTENT as f64).powi(2) * km2_per_unit2;
    // Aim at half the cells filled: high enough to average out, far
    // enough below the one-dot-per-cell ceiling not to clip.
    let cells = (SIZE as f64 / SPACING_PX).powi(2);
    let want_dots = cells * 0.5;
    let density = want_dots / tile_km2;

    let json = format!(
        r##"{{
      "name": "dots",
      "tile-size": {SIZE},
      "sources": {{ "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }} }},
      "nodes": {{
        "bg":    {{ "op": "solid", "color": "#ffffff" }},
        "feats": {{ "op": "features", "source": "src", "layer": "poly" }},
        "dots":  {{ "op": "dot-density", "features": "@feats",
                   "density": {density}, "spacing-px": {SPACING_PX} }},
        "paint": {{ "op": "circles", "features": "@dots", "radius": {RADIUS_PX}, "color": "#000000" }},
        "out":   {{ "op": "blend", "base": "@bg", "over": "@paint" }}
      }},
      "output": "@out"
    }}"##
    );

    let r = render_with_features(
        &json,
        SIZE,
        0,
        // y = 2^z / 2 is the row that starts at the equator.
        TileId {
            z: Z,
            x: 0,
            y: (1 << Z) / 2,
        },
        &[(
            "src.poly",
            layer_of(vec![rect(0, 0, EXTENT - 1, EXTENT - 1)], 1.0),
        )],
    );

    // Dots are 16 px apart and 5 px across, so they don't touch: the
    // dark area is the dot count times one disc.
    let dark = count_dots(&r, 0, 0, SIZE, SIZE) as f64;
    let got = dark / (std::f64::consts::PI * RADIUS_PX * RADIUS_PX);
    // The band is wide because a thresholded disc's pixel footprint is
    // only approximately its area, and half-filled cells carry sampling
    // noise. It is still far tighter than any unit-conversion slip.
    assert!(
        got > want_dots * 0.65 && got < want_dots * 1.35,
        "expected about {want_dots:.0} dots over {tile_km2:.0} km² at {density:.4}/km², \
         found about {got:.0} ({dark:.0} dark pixels)"
    );
}
