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
pub(crate) use symbol::convert_symbol;

pub(crate) fn paint_of(layer: &Map<String, Value>) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    layer
        .get("paint")
        .and_then(Value::as_object)
        .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}
