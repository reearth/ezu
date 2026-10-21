//! `stroke`'s `miter-limit` (MapLibre `line-miter-limit`): how far a miter
//! join may reach before it is bevelled, and, once written, how far an
//! offset line's corners may.

use crate::common::render_with_features;
use ezu_features::{Feature, FeatureLayer, Geometry};
use ezu_graph::{RasterBuf, TileId};
use std::collections::HashMap;
use std::sync::Arc;

/// A V pointing up: (16, 48) → (32, 16) → (48, 48) in tile px. Its legs
/// meet at about 53°, so a miter reaches 1/sin(26.6°) ≈ 2.24 half-widths
/// from the apex: inside a limit of 4, past one of 2.
fn vee() -> FeatureLayer {
    let mut geometry = Geometry::default();
    geometry
        .lines
        .push(vec![(1024, 3072), (2048, 1024), (3072, 3072)]);
    FeatureLayer {
        name: "roads".to_string(),
        extent: 4096,
        features: vec![Feature {
            id: None,
            geometry,
            properties: HashMap::new(),
        }],
    }
}

fn render(extra: &str) -> Arc<RasterBuf> {
    let recipe = format!(
        r##"{{
      "name": "miter",
      "tile-size": 64,
      "sources": {{ "src": {{ "type": "mvt", "url": "http://example.invalid/{{z}}/{{x}}/{{y}}" }} }},
      "nodes": {{
        "feats": {{ "op": "features", "source": "src", "layer": "roads" }},
        "out":   {{ "op": "stroke", "features": "@feats", "color": "#ff0000", "width-px": 8{extra} }}
      }},
      "output": "@out"
    }}"##
    );
    render_with_features(
        &recipe,
        64,
        0,
        TileId { z: 0, x: 0, y: 0 },
        &[("src.roads", vee())],
    )
}

#[test]
fn the_default_keeps_tiny_skias_limit_of_four() {
    assert_eq!(
        render("").pixels,
        render(r#", "miter-limit": 4"#).pixels,
        "an unset limit draws as 4"
    );
}

#[test]
fn a_join_past_the_limit_is_bevelled() {
    // The miter tip reaches 2.24 × 4 px ≈ 9 px above the apex at row 16;
    // a bevel stops about 2 px above it.
    let ink = |r: &RasterBuf| r.pixel(32, 10)[3];
    assert_eq!(ink(&render(r#", "miter-limit": 4"#)), 255, "mitred");
    assert_eq!(ink(&render(r#", "miter-limit": 2.3"#)), 255, "just mitred");
    assert_eq!(ink(&render(r#", "miter-limit": 2"#)), 0, "bevelled");
    assert_eq!(
        ink(&render(r#", "miter-limit": 0.5"#)),
        0,
        "every join bevels"
    );
}

#[test]
fn a_written_limit_also_bounds_offset_corners() {
    // Bevel joins, so only the offset line's corner depends on the limit.
    // Unset, an offset corner keeps MapLibre's default of 2, which this
    // corner's 2.24 passes; a limit of 3 lets it reach its mitred point.
    let bevel = r#", "join": "bevel", "offset-px": 6"#;
    let unset = render(bevel);
    assert_eq!(
        unset.pixels,
        render(&format!(r#"{bevel}, "miter-limit": 2"#)).pixels,
        "an unset limit bounds offset corners at 2"
    );
    assert_ne!(
        unset.pixels,
        render(&format!(r#"{bevel}, "miter-limit": 3"#)).pixels,
        "a limit of 3 mitres the offset corner"
    );
}
