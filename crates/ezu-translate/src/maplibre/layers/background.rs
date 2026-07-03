//! `background` layer → a flat `solid` fill.

use serde_json::{Map, Value};

use crate::maplibre::color::parse_color;
use crate::maplibre::const_color;
use crate::maplibre::layers::paint_of;

pub(crate) fn convert_background(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
) {
    let paint = paint_of(layer);
    // The `solid` node's `color` is a plain port with no `*-expr` sibling,
    // so only a literal `background-color` carries over.
    let (hex, _a) = paint
        .get("background-color")
        .and_then(const_color)
        .and_then(|v| parse_color(&v))
        .unwrap_or_else(|| ("#000000".to_string(), 1.0));
    let nid = format!("{id}__bg");
    nodes.insert(
        nid.clone(),
        serde_json::json!({ "op": "solid", "color": hex }),
    );
    outputs.push(nid);
}
