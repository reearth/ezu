//! `background` layer → a flat `solid` fill.

use serde_json::{Map, Value};

use crate::maplibre::color::{parse_color, with_alpha};
use crate::maplibre::layers::paint_of;
use crate::maplibre::{const_color, const_number, is_expr, Report, ZoomRange};

pub(crate) fn convert_background(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    zoom_range: ZoomRange,
    report: &mut Report,
) {
    let paint = paint_of(layer);
    if paint.get("background-pattern").is_some() {
        report.warn(format!(
            "layer `{id}`: `background-pattern` is not supported — drawn in `background-color`"
        ));
    }
    let opacity = paint.get("background-opacity");
    // A background has no features, so a `background-color` expression can
    // only follow the zoom: an `expr` node evaluates it per tile and feeds
    // the `solid` node's `color`. A literal is the colour itself, with a
    // constant `background-opacity` folded into its alpha.
    let color = match paint.get("background-color") {
        Some(v) if is_expr(v) => {
            if opacity.is_some() {
                report.warn(format!(
                    "layer `{id}`: `background-opacity` with an expression `background-color` is not supported — drawn opaque"
                ));
            }
            let expr_id = format!("{id}__color");
            nodes.insert(
                expr_id.clone(),
                serde_json::json!({ "op": "expr", "type": "color", "expr": v }),
            );
            Value::from(format!("@{expr_id}"))
        }
        v => {
            let (hex, alpha) = match v.and_then(const_color).and_then(|s| parse_color(&s)) {
                Some(c) => c,
                None => {
                    if v.is_some() {
                        report.warn(format!(
                            "layer `{id}`: `background-color` could not be read — drawn black"
                        ));
                    }
                    ("#000000".to_string(), 1.0)
                }
            };
            let opacity = match opacity {
                None => 1.0,
                Some(o) => match const_number(o) {
                    Some(o) => o.clamp(0.0, 1.0) as f32,
                    None => {
                        report.warn(format!(
                            "layer `{id}`: `background-opacity` carries over only as a constant — drawn opaque"
                        ));
                        1.0
                    }
                },
            };
            Value::from(with_alpha(&hex, alpha * opacity))
        }
    };
    let nid = format!("{id}__bg");
    nodes.insert(
        nid.clone(),
        serde_json::json!({ "op": "solid", "color": color }),
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
            "cases": [{ "when": format!("@{zoom}"), "then": format!("@{fill}") }],
            "default": format!("@{off}")
        }),
    );
    gate
}
