//! `background` layer → a flat `solid` fill.

use serde_json::{Map, Value};

use crate::maplibre::color::parse_color;
use crate::maplibre::layers::paint_of;
use crate::maplibre::zoom;
use crate::maplibre::ConvertOptions;

pub(crate) fn convert_background(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    opts: &ConvertOptions,
) {
    let paint = paint_of(layer);
    let (hex, _a) = paint
        .get("background-color")
        .and_then(|v| zoom::color_at(v, opts.zoom))
        .and_then(|v| parse_color(&v))
        .unwrap_or_else(|| ("#000000".to_string(), 1.0));
    let nid = format!("{id}__bg");
    nodes.insert(
        nid.clone(),
        serde_json::json!({ "op": "solid", "color": hex }),
    );
    outputs.push(nid);
}
