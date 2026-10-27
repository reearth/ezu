//! `segment-to`: connectors from a position each feature names in
//! longitude and latitude to the feature's point, or to the edge of its
//! polygon, projected into the tile being rendered.

use crate::common::{features_of_node, render_with_features};
use ezu_core::coord::{world_x_to_lon, world_y_to_lat};
use ezu_features::{Feature, FeatureLayer, Geometry, Value};
use ezu_graph::TileId;
use std::collections::HashMap;

const TILE: TileId = TileId {
    z: 17,
    x: 116_420,
    y: 51_610,
};
const EXTENT: f64 = 4096.0;

/// The longitude and latitude of tile position `(x, y)`.
fn lng_lat(x: f64, y: f64) -> (f64, f64) {
    let n = f64::from(1u32 << TILE.z);
    (
        world_x_to_lon((f64::from(TILE.x) + x / EXTENT) / n),
        world_y_to_lat((f64::from(TILE.y) + y / EXTENT) / n),
    )
}

/// A point at `at` whose other end is tile position `to`, with only the
/// fields named in `fields` set.
fn point(at: (i32, i32), to: (f64, f64), fields: &[&str]) -> Feature {
    let (lng, lat) = lng_lat(to.0, to.1);
    let mut properties = HashMap::new();
    for &f in fields {
        let v = if f == "anchor_lng" { lng } else { lat };
        properties.insert(f.to_string(), Value::Double(v));
    }
    let mut geometry = Geometry::default();
    geometry.points.push(at);
    Feature {
        id: None,
        geometry,
        properties,
    }
}

#[test]
fn connectors_run_from_the_named_position_to_each_point() {
    let both = ["anchor_lng", "anchor_lat"];
    let layer = FeatureLayer {
        name: "entrances".to_string(),
        extent: 4096,
        features: vec![
            // Across the middle, from px (16, 32) to (48, 32).
            point((3072, 2048), (1024.0, 2048.0), &both),
            // Down the middle, but its latitude is missing.
            point((2048, 3584), (2048.0, 512.0), &["anchor_lng"]),
            // From px (16, 16) to a point far east of the tile, as a
            // stitched neighbour supplies it.
            point((6000, 1024), (1024.0, 1024.0), &both),
            // Its other end is the point itself.
            point((1024, 3584), (1024.0, 3584.0), &both),
        ],
    };
    let json = r##"{
      "name": "connectors",
      "tile-size": 64,
      "sources": { "src": { "type": "mvt", "url": "http://example.invalid/{z}/{x}/{y}" } },
      "nodes": {
        "bg":    { "op": "solid", "color": "#ffffff" },
        "pts":   { "op": "features", "source": "src", "layer": "entrances" },
        "links": { "op": "segment-to", "features": "@pts",
                   "lng-field": "anchor_lng", "lat-field": "anchor_lat" },
        "draw":  { "op": "stroke", "features": "@links", "color": "#000000", "width-px": 2 },
        "out":   { "op": "blend", "base": "@bg", "over": "@draw" }
      },
      "output": "@out"
    }"##;
    let r = render_with_features(json, 64, 0, TILE, &[("src.entrances", layer)]);
    let dark = |x: u32, y: u32| r.pixel(x, y)[0] < 128;
    assert!(dark(20, 32) && dark(44, 32), "connector across the middle");
    assert!(!dark(10, 32) && !dark(54, 32), "it stops at its two ends");
    assert!(
        !dark(32, 20) && !dark(32, 44),
        "no connector without a latitude"
    );
    assert!(
        dark(40, 16) && dark(62, 16),
        "connector from outside the tile"
    );
    assert!(!dark(10, 16), "starting at its named end");
}

/// A square from `(x0, y0)` to `(x1, y1)` whose other end is tile position
/// `to`.
fn square(x0: i32, y0: i32, x1: i32, y1: i32, to: (f64, f64)) -> Feature {
    let (lng, lat) = lng_lat(to.0, to.1);
    let mut properties = HashMap::new();
    properties.insert("anchor_lng".to_string(), Value::Double(lng));
    properties.insert("anchor_lat".to_string(), Value::Double(lat));
    let mut geometry = Geometry::default();
    geometry.polygons.push(ezu_features::Polygon {
        exterior: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)],
        holes: vec![],
    });
    Feature {
        id: None,
        geometry,
        properties,
    }
}

/// The lines `segment-to` makes from `features`, with `extra` added to
/// its fields, on a 64 px tile (64 extent units a pixel).
fn connectors(features: Vec<Feature>, extra: &str) -> Vec<Vec<(i32, i32)>> {
    let layer = FeatureLayer {
        name: "boxes".to_string(),
        extent: 4096,
        features,
    };
    let json = format!(
        r##"{{
      "name": "connectors",
      "tile-size": 64,
      "sources": {{ "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }} }},
      "nodes": {{
        "bg":    {{ "op": "solid", "color": "#ffffff" }},
        "feats": {{ "op": "features", "source": "src", "layer": "boxes" }},
        "links": {{ "op": "segment-to", "features": "@feats",
                    "lng-field": "anchor_lng", "lat-field": "anchor_lat" {extra} }},
        "draw":  {{ "op": "stroke", "features": "@links", "color": "#000000", "width-px": 2 }},
        "out":   {{ "op": "blend", "base": "@bg", "over": "@draw" }}
      }},
      "output": "@out"
    }}"##
    );
    let (feats, _) = features_of_node(&json, 64, 0, TILE, &[("src.boxes", layer)], "links");
    feats
        .groups
        .iter()
        .flat_map(|g| g.lines.iter().cloned())
        .collect()
}

/// Whether `a` and `b` are within one extent unit of each other on both
/// axes, the most rounding the ends can pick up.
fn near(a: (i32, i32), b: (i32, i32)) -> bool {
    (a.0 - b.0).abs() <= 1 && (a.1 - b.1).abs() <= 1
}

#[test]
fn a_connector_to_a_polygon_stops_at_its_edge() {
    let lines = connectors(
        vec![
            // From the left, level with the middle: ends on the left edge.
            square(2048, 1024, 3072, 2048, (512.0, 1536.0)),
            // From below and to the right: ends on the bottom edge, on the
            // way to the centre (2560, 1536).
            square(2048, 1024, 3072, 2048, (3072.0, 3584.0)),
        ],
        "",
    );
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0][0], (512, 1536));
    assert!(near(lines[0][1], (2048, 1536)), "{lines:?}");
    assert_eq!(lines[1][0], (3072, 3584));
    // (3072, 3584) to (2560, 1536) crosses y = 2048 a quarter of the way.
    assert!(near(lines[1][1], (2688, 2048)), "{lines:?}");
}

#[test]
fn a_polygon_around_its_other_end_gets_no_connector() {
    let lines = connectors(
        vec![
            square(1024, 1024, 3072, 3072, (2000.0, 2500.0)),
            // On the edge.
            square(1024, 1024, 3072, 3072, (1024.0, 2000.0)),
        ],
        "",
    );
    assert!(lines.is_empty(), "{lines:?}");
}

#[test]
fn gap_px_pulls_the_feature_end_back() {
    // 2 px is 128 extent units on a 64 px tile.
    let lines = connectors(
        vec![
            square(2048, 1024, 3072, 2048, (512.0, 1536.0)),
            point(
                (3072, 3072),
                (1024.0, 3072.0),
                &["anchor_lng", "anchor_lat"],
            ),
            // Shorter than the gap: dropped.
            point((1100, 512), (1024.0, 512.0), &["anchor_lng", "anchor_lat"]),
        ],
        r#", "gap-px": 2"#,
    );
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0][0], (512, 1536));
    assert!(near(lines[0][1], (2048 - 128, 1536)), "{lines:?}");
    assert_eq!(lines[1], vec![(1024, 3072), (3072 - 128, 3072)]);
}

#[test]
fn a_leader_runs_from_the_feature_to_the_edge_of_its_moved_label() {
    // Three labels on one point, each position further up: the second and
    // third move off the first. Their text boxes, picked out by
    // `variant-index`, each get a leader from the labelled point that stops
    // at the box.
    let tile = TileId { z: 0, x: 0, y: 0 };
    let at = (2048, 3072);
    // Tile 0/0/0 spans the world, so its frame is the world's scaled up.
    let (lng, lat) = (
        world_x_to_lon(f64::from(at.0) / EXTENT),
        world_y_to_lat(f64::from(at.1) / EXTENT),
    );
    let label = |name: &str| {
        let mut properties = HashMap::new();
        properties.insert("name".to_string(), Value::String(name.to_string()));
        properties.insert("lng".to_string(), Value::Double(lng));
        properties.insert("lat".to_string(), Value::Double(lat));
        let mut geometry = Geometry::default();
        geometry.points.push(at);
        Feature {
            id: None,
            geometry,
            properties,
        }
    };
    let layer = FeatureLayer {
        name: "pts".to_string(),
        extent: 4096,
        features: vec![label("AA"), label("BB"), label("CC")],
    };
    let font = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
    let font = format!("file:{}", font.display()).replace('\\', "/");
    let json = format!(
        r##"{{
      "name": "leaders",
      "tile-size": 128,
      "sources": {{
        "src":  {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }},
        "body": {{ "type": "font", "url": "{font}" }}
      }},
      "nodes": {{
        "feats":   {{ "op": "features", "source": "src", "layer": "pts" }},
        "labels":  {{ "op": "text-labels", "features": "@feats",
                      "font": ["body"], "text": ["get", "name"], "size": 16, "color": "#000000",
                      "anchor-offsets": [
                        {{ "anchor": "bottom", "offset-em": [0, -0.5] }},
                        {{ "anchor": "bottom", "offset-em": [0, -2.5] }},
                        {{ "anchor": "bottom", "offset-em": [0, -4.5] }}
                      ] }},
        "placed":  {{ "op": "label-placement", "labels": ["@labels"] }},
        "boxes":   {{ "op": "label-boxes", "placement": "@placed", "parts": ["text"],
                      "filter-expr": [">", ["coalesce", ["get", "variant-index"], 0], 0] }},
        "leaders": {{ "op": "segment-to", "features": "@boxes",
                      "lng-field": "lng", "lat-field": "lat" }},
        "draw":    {{ "op": "stroke", "features": "@leaders", "color": "#000000", "width-px": 1 }},
        "text":    {{ "op": "text-draw", "labels": "@labels", "placement": "@placed" }},
        "out":     {{ "op": "stack", "layers": ["@draw", "@text"] }}
      }},
      "output": "@out"
    }}"##
    );
    let bindings = [("src.pts", layer)];
    let (boxes, _) = features_of_node(&json, 128, 0, tile, &bindings, "boxes");
    let (leaders, _) = features_of_node(&json, 128, 0, tile, &bindings, "leaders");
    let mut moved: Vec<_> = boxes
        .groups
        .iter()
        .map(|g| match g.properties.get("variant-index") {
            Some(maplibre_expr::Value::Number(n)) => *n,
            other => panic!("variant-index: {other:?}"),
        })
        .collect();
    moved.sort_by(f64::total_cmp);
    assert_eq!(moved, [1.0, 2.0], "the two labels that moved");
    assert_eq!(leaders.groups.len(), 2, "one leader per box");
    for (b, l) in boxes.groups.iter().zip(&leaders.groups) {
        assert_eq!(
            b.properties.get("variant-index"),
            l.properties.get("variant-index")
        );
        let ring = &b.polygons[0].exterior;
        let (x0, x1) = (
            ring.iter().map(|p| p.0).min().unwrap(),
            ring.iter().map(|p| p.0).max().unwrap(),
        );
        let bottom = ring.iter().map(|p| p.1).max().unwrap();
        let line = &l.lines[0];
        assert!(near(line[0], at), "starts at the point: {line:?}");
        // Straight up from the point, the leader ends on the box's bottom.
        assert!(
            near(line[1], (at.0, bottom)) && line[1].0 >= x0 && line[1].0 <= x1,
            "{line:?} vs {ring:?}"
        );
    }
}
