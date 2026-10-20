//! `hillshade` layer over a `raster-dem` source → `dem` + `hillshade` nodes.

use serde_json::{Map, Value};

use crate::maplibre::color::parse_color;
use crate::maplibre::layers::{paint_of, with_zoom_range};
use crate::maplibre::{const_color, const_number};
use crate::maplibre::{Report, ZoomRange};

/// A `hillshade` layer over a `raster-dem` source → an ezu `dem` node
/// feeding a `hillshade` node. ezu already has the whole terrain stack
/// (`dem` / `hillshade` / `slope` / `color-ramp`); this just wires the
/// MapLibre paint props onto it.
pub(crate) fn convert_hillshade(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    zoom_range: ZoomRange,
    report: &mut Report,
) {
    let Some(src) = layer.get("source").and_then(Value::as_str) else {
        report.warn(format!("layer `{id}`: hillshade without source — skipped"));
        return;
    };
    let paint = paint_of(layer);
    // MapLibre defaults: illumination-direction 335°, exaggeration 0.5. The
    // `hillshade` node's params are plain ports (no `*-expr`), so only a
    // literal value carries over.
    let azimuth = paint
        .get("hillshade-illumination-direction")
        .and_then(const_number)
        .unwrap_or(335.0);
    let exaggeration = paint
        .get("hillshade-exaggeration")
        .and_then(const_number)
        .unwrap_or(0.5);

    // Shadow and highlight map onto `relief`'s own colours. They are plain
    // ports, so only a literal colour carries over. Left unset, the node
    // keeps its defaults (black shadow, no highlight) rather than
    // MapLibre's white highlight, so a style that does not name them
    // renders as it always has.
    let mut colours = Vec::new();
    for (prop, field) in [
        ("hillshade-shadow-color", "shadow-color"),
        ("hillshade-highlight-color", "highlight-color"),
    ] {
        let Some(v) = paint.get(prop) else {
            continue;
        };
        match const_color(v).and_then(|s| parse_color(&s)) {
            Some((hex, alpha)) => colours.push((field, with_alpha(&hex, alpha))),
            None => report.warn(format!(
                "layer `{id}`: `{prop}` carries over only as a constant colour — \
                 left at the hillshade node's default"
            )),
        }
    }

    let dem_id = format!("{id}__dem");
    let hs_id = format!("{id}__hillshade");
    // Outside the band the `dem` node reads a zero field, which `relief`
    // turns into a fully transparent layer.
    nodes.insert(
        dem_id.clone(),
        with_zoom_range(
            serde_json::json!({ "op": "dem", "source": src }),
            zoom_range,
        ),
    );
    let mut hillshade = serde_json::json!({
            "op": "hillshade",
            "field": format!("@{dem_id}"),
            "azimuth-deg": azimuth,
            "altitude-deg": 45,
            "exaggeration": exaggeration,
            // `relief` leaves flat ground white and only darkens slopes,
            // matching MapLibre's hillshade look over a light background
            // (vs `shade`, which greys flat ground too).
            "mode": "relief"
    });
    for (field, colour) in colours {
        hillshade[field] = Value::from(colour);
    }
    nodes.insert(hs_id.clone(), hillshade);
    outputs.push(hs_id);
}

/// `#rrggbb` with `alpha` folded in as `#rrggbbaa`, or left as it is when
/// opaque.
fn with_alpha(hex: &str, alpha: f32) -> String {
    if alpha >= 1.0 {
        return hex.to_string();
    }
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("{hex}{a:02x}")
}
