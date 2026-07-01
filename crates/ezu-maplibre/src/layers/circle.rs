//! `circle` layer → a `circle` sprite `stamp`ed at each point feature.

use serde_json::{Map, Value};

use crate::color::parse_color;
use crate::filter;
use crate::layers::paint_of;
use crate::sources::{features_node, resolve_layer_source, Sources};
use crate::zoom;
use crate::{ConvertOptions, Report};

/// A `circle` layer → a `circle` sprite `stamp`ed at each point feature.
/// `circle-stroke-*` is a second, larger disk stamped underneath (the ring
/// shows around the fill).
pub(crate) fn convert_circle(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    opts: &ConvertOptions,
    sources: &Sources,
    report: &mut Report,
) {
    let Some((source, source_layer)) = resolve_layer_source(id, layer, sources, report) else {
        return;
    };
    let base_filter = layer
        .get("filter")
        .and_then(|f| filter::convert(f, report, id));
    let paint = paint_of(layer);

    let num = |key: &str, default: f64| {
        paint
            .get(key)
            .and_then(|v| zoom::number_at(v, opts.zoom))
            .unwrap_or(default)
    };
    let color = |key: &str, default: &str| {
        paint
            .get(key)
            .and_then(|v| zoom::color_at(v, opts.zoom))
            .and_then(|v| parse_color(&v))
            .map(|(hex, _)| hex)
            .unwrap_or_else(|| default.to_string())
    };

    let radius = num("circle-radius", 5.0).max(0.1);
    let fill = color("circle-color", "#000000");
    let opacity = paint
        .get("circle-opacity")
        .and_then(|v| zoom::number_at(v, opts.zoom));
    let stroke_w = num("circle-stroke-width", 0.0);

    let feat_id = format!("{id}__feat");
    nodes.insert(
        feat_id.clone(),
        features_node(&source, &source_layer, base_filter),
    );

    // Stroke ring first (drawn under), then the fill disk on top.
    if stroke_w > 0.0 {
        let sc = color("circle-stroke-color", "#000000");
        emit_disk(
            id,
            "stroke",
            &feat_id,
            radius + stroke_w,
            sc,
            opacity,
            nodes,
            outputs,
        );
    }
    emit_disk(id, "fill", &feat_id, radius, fill, opacity, nodes, outputs);
}

/// Emit a `circle` sprite of pixel `radius` + a `stamp` placing it at each
/// point of `feat_id`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_disk(
    id: &str,
    suffix: &str,
    feat_id: &str,
    radius: f64,
    hex: String,
    opacity: Option<f64>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
) {
    // 1px margin around the disk so its antialiased edge isn't clipped.
    let size = ((2.0 * radius).ceil() as i64 + 2).max(1);
    let radius_frac = radius / size as f64;
    let circle_id = format!("{id}__{suffix}_circle");
    nodes.insert(
        circle_id.clone(),
        serde_json::json!({
            "op": "circle", "kind": "sprite", "color": hex,
            "radius-frac": radius_frac, "width-px": size, "height-px": size
        }),
    );
    let stamp_id = format!("{id}__{suffix}_stamp");
    let mut spec = serde_json::json!({
        "op": "stamp", "features": format!("@{feat_id}"), "image": format!("@{circle_id}")
    });
    if let Some(a) = opacity {
        spec["opacity"] = Value::from(a);
    }
    nodes.insert(stamp_id.clone(), spec);
    outputs.push(stamp_id);
}
