//! `background` layer → a flat `solid` fill.

use serde_json::{Map, Value};

use crate::maplibre::color::parse_color;
use crate::maplibre::layers::paint_of;
use crate::maplibre::{const_color, ZoomRange};

pub(crate) fn convert_background(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    zoom_range: ZoomRange,
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
    outputs.push(gate_on_zoom(id, nid, nodes, zoom_range));
}

/// `solid` has no zoom gate of its own, so a background with a zoom band
/// becomes a `switch` between it and a transparent fill, picked per tile
/// by an `expr` over the zoom. Without a band the fill is used as it is.
fn gate_on_zoom(
    id: &str,
    fill: String,
    nodes: &mut Map<String, Value>,
    (min, max): ZoomRange,
) -> String {
    // MapLibre admits `["zoom"]` only as the input of a top-level `step`
    // (or `interpolate`), so the band is a step: off below `min`, on from
    // it, off again past the inclusive `max`.
    let mut expr = vec![serde_json::json!("step"), serde_json::json!(["zoom"])];
    match (min, max) {
        (None, None) => return fill,
        (Some(mn), _) => expr.extend([Value::from(false), Value::from(mn), Value::from(true)]),
        (None, Some(_)) => expr.push(Value::from(true)),
    }
    if let Some(mx) = max {
        expr.extend([Value::from(mx + 1), Value::from(false)]);
    }
    let (zoom, off, gate) = (
        format!("{id}__zoom"),
        format!("{id}__off"),
        format!("{id}__zoom_gate"),
    );
    nodes.insert(
        zoom.clone(),
        serde_json::json!({ "op": "expr", "type": "bool", "expr": expr }),
    );
    nodes.insert(
        off.clone(),
        serde_json::json!({ "op": "solid", "color": "#00000000" }),
    );
    nodes.insert(
        gate.clone(),
        serde_json::json!({
            "op": "switch",
            "a": format!("@{off}"),
            "b": format!("@{fill}"),
            "select": format!("@{zoom}")
        }),
    );
    gate
}
