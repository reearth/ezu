//! Per-layer-type converters. Each MapLibre layer `type` lowers to a small
//! subgraph of ezu nodes; the modules here hold one concern each and the
//! dispatch loop in [`crate::maplibre::convert`] calls into them.

use serde_json::{Map, Value};

pub(crate) mod background;
pub(crate) mod circle;
pub(crate) mod fill;
pub(crate) mod heatmap;
pub(crate) mod hillshade;
pub(crate) mod line;
pub(crate) mod raster;
pub(crate) mod symbol;

pub(crate) use background::convert_background;
pub(crate) use circle::convert_circle;
pub(crate) use fill::{convert_fill, convert_fill_extrusion};
pub(crate) use heatmap::convert_heatmap;
pub(crate) use hillshade::convert_hillshade;
pub(crate) use line::convert_line;
pub(crate) use raster::convert_raster;
pub(crate) use symbol::{convert_symbol, emit_label_placement};

/// Put a layer's zoom band on a node that gates on `min-zoom`/`max-zoom`
/// itself (`raster`, `dem`), leaving out a bound the layer does not set.
pub(crate) fn with_zoom_range(mut node: Value, (min, max): crate::maplibre::ZoomRange) -> Value {
    if let Some(z) = min {
        node["min-zoom"] = Value::from(z);
    }
    if let Some(z) = max {
        node["max-zoom"] = Value::from(z);
    }
    node
}

/// MapLibre's `<prefix>-translate` (`circle-translate`, `fill-translate`,
/// `line-translate`): shift the layer's geometry by a constant `[x, y]` in
/// pixels, through a `transform` node between `features` (the `@id` the
/// paint node would read) and the paint node. Returns the reference the
/// paint node reads instead — `features` itself when there is nothing to
/// shift.
///
/// `<prefix>-translate-anchor` needs nothing: it only says whether the
/// shift turns with the map, and a tile is never turned.
pub(crate) fn translated(
    id: &str,
    prop: &str,
    paint: &Map<String, Value>,
    features: String,
    nodes: &mut Map<String, Value>,
    report: &mut crate::maplibre::Report,
) -> String {
    let Some(v) = paint.get(prop) else {
        return features;
    };
    let pair = match v.as_array().map(Vec::as_slice) {
        Some([Value::String(op), Value::Array(inner)]) if op == "literal" => Some(inner.as_slice()),
        Some(arr) => Some(arr),
        None => None,
    }
    .and_then(|arr| match arr {
        [x, y] => Some([x.as_f64()?, y.as_f64()?]),
        _ => None,
    });
    let Some([x, y]) = pair else {
        report.warn(format!(
            "layer `{id}`: `{prop}` carries over only as a constant `[x, y]` — drawn unshifted"
        ));
        return features;
    };
    if x == 0.0 && y == 0.0 {
        return features;
    }
    let shift_id = format!("{id}__translate");
    nodes.insert(
        shift_id.clone(),
        serde_json::json!({ "op": "transform", "features": features, "translate-px": [x, y] }),
    );
    format!("@{shift_id}")
}

pub(crate) fn paint_of(layer: &Map<String, Value>) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    layer
        .get("paint")
        .and_then(Value::as_object)
        .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}
