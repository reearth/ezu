//! `line` layer → a crisp `stroke` (or `line-stamp` when `line-pattern`
//! is set).

use serde_json::{Map, Value};

use crate::color::parse_color;
use crate::filter;
use crate::layers::paint_of;
use crate::sources::{features_node, resolve_layer_source, Sources};
use crate::zoom;
use crate::{ConvertOptions, Report};

pub(crate) fn convert_line(
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

    let width = paint
        .get("line-width")
        .and_then(|v| zoom::number_at(v, opts.zoom))
        .unwrap_or(1.0)
        .max(0.1);

    // `line-pattern` replaces the solid stroke: repeat the named sprite icon
    // along each line, scaled to the stroke width (`line-stamp`).
    if let Some(pattern) = paint.get("line-pattern") {
        let opacity = paint
            .get("line-opacity")
            .and_then(|v| zoom::number_at(v, opts.zoom));
        convert_line_pattern(
            id,
            pattern,
            &source,
            &source_layer,
            base_filter,
            width,
            opacity,
            sources,
            nodes,
            outputs,
            report,
        );
        return;
    }

    let (hex, _a) = paint
        .get("line-color")
        .and_then(|v| zoom::color_at(v, opts.zoom))
        .and_then(|v| parse_color(&v))
        .unwrap_or_else(|| ("#000000".to_string(), 1.0));
    let opacity = paint
        .get("line-opacity")
        .and_then(|v| zoom::number_at(v, opts.zoom));

    // `layout.line-cap` / `-join` (MapLibre defaults: butt / miter).
    let layout = layer.get("layout").and_then(Value::as_object);
    let cap = layout
        .and_then(|l| l.get("line-cap"))
        .and_then(Value::as_str)
        .unwrap_or("butt");
    let join = layout
        .and_then(|l| l.get("line-join"))
        .and_then(Value::as_str)
        .unwrap_or("miter");

    let feat_id = format!("{id}__feat");
    let stroke_id = format!("{id}__stroke");
    nodes.insert(
        feat_id.clone(),
        features_node(&source, &source_layer, base_filter),
    );
    // Crisp `stroke` (tiny-skia) rather than a painterly brush, to match
    // MapLibre's clean vector lines.
    let mut spec = serde_json::json!({
        "op": "stroke",
        "features": format!("@{feat_id}"),
        "color": hex,
        "width-px": width,
        "cap": cap,
        "join": join,
    });
    if let Some(a) = opacity {
        spec["opacity"] = Value::from(a);
    }
    // MapLibre `line-dasharray` is in units of line width → convert to px.
    if let Some(arr) = paint.get("line-dasharray").and_then(Value::as_array) {
        let dash: Vec<Value> = arr
            .iter()
            .filter_map(Value::as_f64)
            .map(|d| Value::from(d * width))
            .collect();
        if !dash.is_empty() {
            spec["dasharray"] = Value::Array(dash);
        }
    }
    nodes.insert(stroke_id.clone(), spec);
    outputs.push(stroke_id);
}

/// `line-pattern` → repeat the named sprite icon along each line, fit to the
/// stroke width: `features` → `icon` → `line-stamp`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn convert_line_pattern(
    id: &str,
    pattern: &Value,
    source: &str,
    source_layer: &str,
    base_filter: Option<Map<String, Value>>,
    width: f64,
    opacity: Option<f64>,
    sources: &Sources,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    report: &mut Report,
) {
    let Some(name) = pattern.as_str() else {
        report.warn(format!(
            "layer `{id}`: data-driven `line-pattern` not supported — skipped"
        ));
        return;
    };
    let Some((sprite_src, icon_name)) = sources.resolve_icon(name) else {
        report.warn(format!(
            "layer `{id}`: line-pattern `{name}` needs a `sprite`, but the style declares none — skipped"
        ));
        return;
    };
    let feat_id = format!("{id}__feat");
    nodes.insert(
        feat_id.clone(),
        features_node(source, source_layer, base_filter),
    );
    let icon_id = format!("{id}__icon");
    nodes.insert(
        icon_id.clone(),
        serde_json::json!({ "op": "icon", "sprite": format!("@{sprite_src}"), "name": icon_name }),
    );
    let out_id = format!("{id}__linepat");
    let mut spec = serde_json::json!({
        "op": "line-stamp", "features": format!("@{feat_id}"),
        "image": format!("@{icon_id}"), "width-px": width
    });
    if let Some(a) = opacity {
        spec["opacity"] = Value::from(a);
    }
    nodes.insert(out_id.clone(), spec);
    outputs.push(out_id);
}
