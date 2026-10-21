//! A paint or layout property the converter neither carries over nor needs
//! is named in a warning, so a style never draws differently from MapLibre
//! without a word; one that changes nothing on a tile stays quiet.

use ezu_translate::maplibre::{convert, ConvertOptions};
use serde_json::{json, Value};

/// The warnings converting a style with the one `layer` gives.
fn warnings_for(layer: Value) -> Vec<String> {
    let style = json!({
        "version": 8,
        "name": "unread",
        "glyphs": "https://example.com/fonts/{fontstack}/{range}.pbf",
        "sources": {
            "s": { "type": "vector", "url": "https://example.com/tiles.json" },
            "r": { "type": "raster", "tiles": ["https://example.com/{z}/{x}/{y}.png"] }
        },
        "layers": [layer]
    });
    convert(&style, &ConvertOptions::default())
        .unwrap()
        .1
        .warnings
}

fn names_unsupported(warnings: &[String], prop: &str) -> bool {
    let needle = format!("`{prop}` is not supported");
    warnings.iter().any(|w| w.contains(&needle))
}

#[test]
fn a_property_nothing_reads_warns() {
    let w = warnings_for(json!({
        "id": "l", "type": "line", "source": "s", "source-layer": "x",
        "layout": { "line-miter-limit": 3, "line-sort-key": ["get", "rank"] },
        "paint": { "line-blur": 1, "line-gradient": ["interpolate", ["linear"], ["line-progress"], 0, "red", 1, "blue"] }
    }));
    for prop in [
        "line-miter-limit",
        "line-sort-key",
        "line-blur",
        "line-gradient",
    ] {
        assert!(names_unsupported(&w, prop), "{prop}: {w:?}");
    }

    let w = warnings_for(json!({
        "id": "t", "type": "symbol", "source": "s", "source-layer": "x",
        "layout": { "text-field": "x" },
        "paint": { "text-halo-blur": 1 }
    }));
    assert!(names_unsupported(&w, "text-halo-blur"), "{w:?}");

    let w = warnings_for(json!({
        "id": "f", "type": "fill", "source": "s", "source-layer": "x",
        "paint": { "fill-color": "#888", "fill-antialias": false }
    }));
    assert!(w.iter().any(|w| w.contains("fill-antialias")), "{w:?}");
}

#[test]
fn what_changes_nothing_on_a_tile_stays_quiet() {
    let w = warnings_for(json!({
        "id": "c", "type": "circle", "source": "s", "source-layer": "x",
        "layout": { "visibility": "visible" },
        "paint": {
            "circle-radius": 3, "circle-pitch-scale": "viewport",
            "circle-pitch-alignment": "map", "circle-translate-anchor": "viewport"
        }
    }));
    assert!(w.is_empty(), "{w:?}");

    // A symbol's icon knobs without an icon, as MapLibre ignores them, and
    // how a point-placed icon turns with the map.
    let w = warnings_for(json!({
        "id": "t", "type": "symbol", "source": "s", "source-layer": "x",
        "layout": {
            "text-field": "x", "symbol-avoid-edges": true,
            "text-pitch-alignment": "viewport", "icon-keep-upright": true
        },
        "paint": { "icon-color": "#f00", "icon-halo-width": 1 }
    }));
    assert!(w.is_empty(), "{w:?}");

    // A fill drawn anti-aliased, as ezu always does.
    let w = warnings_for(json!({
        "id": "f", "type": "fill", "source": "s", "source-layer": "x",
        "paint": { "fill-color": "#888", "fill-antialias": true }
    }));
    assert!(w.is_empty(), "{w:?}");

    // Every raster paint property is carried over, and a fade in time
    // has nothing to fade on a static tile.
    let w = warnings_for(json!({
        "id": "r", "type": "raster", "source": "r",
        "paint": {
            "raster-opacity": 0.5, "raster-saturation": -1, "raster-contrast": 0.2,
            "raster-hue-rotate": 30, "raster-brightness-min": 0.1,
            "raster-brightness-max": 0.9, "raster-resampling": "nearest",
            "raster-fade-duration": 0
        }
    }));
    assert!(w.is_empty(), "{w:?}");
}
