//! `hillshade` layer over a `raster-dem` source → `dem` + `hillshade` nodes.

use serde_json::{Map, Value};

use crate::maplibre::const_number;
use crate::maplibre::layers::{paint_of, with_zoom_range};
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
    nodes.insert(
        hs_id.clone(),
        serde_json::json!({
            "op": "hillshade",
            "field": format!("@{dem_id}"),
            "azimuth-deg": azimuth,
            "altitude-deg": 45,
            "exaggeration": exaggeration,
            // `relief` leaves flat ground white and only darkens slopes,
            // matching MapLibre's hillshade look over a light background
            // (vs `shade`, which greys flat ground too).
            "mode": "relief"
        }),
    );
    outputs.push(hs_id);
}
