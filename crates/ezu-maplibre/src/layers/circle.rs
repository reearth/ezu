//! `circle` layer → a single `circles` paint node: a filled disk per point
//! feature, with per-feature radius / color / opacity / stroke routed to the
//! node's `*-expr` fields when the paint property is a data-driven expression.

use serde_json::{Map, Value};

use crate::filter;
use crate::layers::fill::{resolve_number, resolve_paint_color};
use crate::layers::paint_of;
use crate::sources::{features_node, resolve_layer_source, Sources};
use crate::{ConvertOptions, Report};

/// A `circle` layer → one `circles` node. Each paint prop bakes to a constant
/// when zoom-resolvable, else its raw expression is emitted as a `*-expr`.
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
    let (base_filter, base_filter_expr) = filter::layer_filters(layer, report, id);
    let paint = paint_of(layer);

    let feat_id = format!("{id}__feat");
    nodes.insert(
        feat_id.clone(),
        features_node(&source, &source_layer, base_filter, base_filter_expr),
    );

    let circles_id = format!("{id}__circles");
    let mut spec = serde_json::json!({
        "op": "circles",
        "features": format!("@{feat_id}"),
    });

    // `circle-radius` → `radius` (constant) or `radius-expr`.
    let (radius, radius_expr) = resolve_number(paint.get("circle-radius"), opts.zoom);
    if let Some(r) = radius {
        spec["radius"] = Value::from(r.max(0.0));
    }
    if let Some(e) = radius_expr {
        spec["radius-expr"] = e;
    }

    // `circle-color` → `color` (constant hex) or `color-expr`.
    let (color_hex, color_expr) = resolve_paint_color(paint.get("circle-color"), opts.zoom);
    if let Some(hex) = color_hex {
        spec["color"] = Value::from(hex);
    }
    if let Some(e) = color_expr {
        spec["color-expr"] = e;
    }

    // `circle-opacity` → `opacity` (constant) or `opacity-expr`.
    let (opacity, opacity_expr) = resolve_number(paint.get("circle-opacity"), opts.zoom);
    if let Some(o) = opacity {
        spec["opacity"] = Value::from(o);
    }
    if let Some(e) = opacity_expr {
        spec["opacity-expr"] = e;
    }

    // `circle-stroke-width` → `stroke-width` (constant) or `stroke-width-expr`.
    let (sw, sw_expr) = resolve_number(paint.get("circle-stroke-width"), opts.zoom);
    if let Some(w) = sw {
        spec["stroke-width"] = Value::from(w.max(0.0));
    }
    if let Some(e) = sw_expr {
        spec["stroke-width-expr"] = e;
    }

    // `circle-stroke-color` → `stroke-color` (constant hex) or
    // `stroke-color-expr`.
    let (sc_hex, sc_expr) = resolve_paint_color(paint.get("circle-stroke-color"), opts.zoom);
    if let Some(hex) = sc_hex {
        spec["stroke-color"] = Value::from(hex);
    }
    if let Some(e) = sc_expr {
        spec["stroke-color-expr"] = e;
    }

    nodes.insert(circles_id.clone(), spec);
    outputs.push(circles_id);
}
