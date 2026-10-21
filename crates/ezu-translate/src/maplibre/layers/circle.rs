//! `circle` layer → a single `circles` paint node: a filled disk per point
//! feature, with per-feature radius / color / opacity / stroke routed to the
//! node's `*-expr` fields when the paint property is a data-driven expression.

use serde_json::{Map, Value};

use crate::maplibre::filter;
use crate::maplibre::layers::fill::{resolve_number, resolve_paint_color, warn_unreadable_color};
use crate::maplibre::layers::{paint_of, translated};
use crate::maplibre::sources::{features_node, resolve_layer_source, Sources};
use crate::maplibre::{Report, ZoomRange};

/// A `circle` layer → one `circles` node. Each paint prop is a constant when
/// it's a literal, else its raw expression is emitted as a `*-expr`.
pub(crate) fn convert_circle(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    zoom_range: ZoomRange,
    sources: &Sources,
    report: &mut Report,
) {
    let Some((source, source_layer)) = resolve_layer_source(id, layer, sources, report) else {
        return;
    };
    let (min_zoom, max_zoom) = zoom_range;
    let base_filter_expr = filter::layer_filter_expr(layer, report, id);
    let paint = paint_of(layer);

    let feat_id = format!("{id}__feat");
    nodes.insert(
        feat_id.clone(),
        features_node(&source, &source_layer, base_filter_expr, min_zoom, max_zoom),
    );

    let features = translated(
        id,
        "circle-translate",
        paint,
        format!("@{feat_id}"),
        nodes,
        report,
    );
    let circles_id = format!("{id}__circles");
    let mut spec = serde_json::json!({
        "op": "circles",
        "features": features,
    });

    // `circle-radius` → `radius` (constant) or `radius-expr`.
    let (radius, radius_expr) = resolve_number(paint.get("circle-radius"));
    if let Some(r) = radius {
        spec["radius"] = Value::from(r.max(0.0));
    }
    if let Some(e) = radius_expr {
        spec["radius-expr"] = e;
    }

    // `circle-color` → `color` (constant hex) or `color-expr`. Left unset,
    // the node's black is MapLibre's default too, and backs an expression
    // for a feature it gives no colour.
    let (color_hex, color_expr) = resolve_paint_color(paint.get("circle-color"));
    warn_unreadable_color(
        id,
        "circle-color",
        paint.get("circle-color"),
        &color_hex,
        &color_expr,
        report,
    );
    if let Some(hex) = color_hex {
        spec["color"] = Value::from(hex);
    }
    if let Some(e) = color_expr {
        spec["color-expr"] = e;
    }

    // `circle-opacity` → `opacity` (constant) or `opacity-expr`.
    let (opacity, opacity_expr) = resolve_number(paint.get("circle-opacity"));
    if let Some(o) = opacity {
        spec["opacity"] = Value::from(o);
    }
    if let Some(e) = opacity_expr {
        spec["opacity-expr"] = e;
    }

    // `circle-stroke-width` → `stroke-width` (constant) or `stroke-width-expr`.
    let (sw, sw_expr) = resolve_number(paint.get("circle-stroke-width"));
    if let Some(w) = sw {
        spec["stroke-width"] = Value::from(w.max(0.0));
    }
    if let Some(e) = sw_expr {
        spec["stroke-width-expr"] = e;
    }

    // `circle-stroke-color` → `stroke-color` (constant hex) or
    // `stroke-color-expr`.
    let (sc_hex, sc_expr) = resolve_paint_color(paint.get("circle-stroke-color"));
    warn_unreadable_color(
        id,
        "circle-stroke-color",
        paint.get("circle-stroke-color"),
        &sc_hex,
        &sc_expr,
        report,
    );
    if let Some(hex) = sc_hex {
        spec["stroke-color"] = Value::from(hex);
    }
    if let Some(e) = sc_expr {
        spec["stroke-color-expr"] = e;
    }

    // `circle-stroke-opacity` → `stroke-opacity` (constant) or
    // `stroke-opacity-expr`. MapLibre's `circle-opacity` fades the fill
    // alone, while the node's `opacity` also fades the ring unless the ring
    // has an opacity of its own — so a layer that sets `circle-opacity` and
    // draws a ring gets the default `stroke-opacity` of 1 spelled out.
    let (so, so_expr) = resolve_number(paint.get("circle-stroke-opacity"));
    let has_ring = sw.is_some_and(|w| w > 0.0) || spec.get("stroke-width-expr").is_some();
    // MapLibre draws the ring outside `circle-radius`, which keeps its
    // full fill: the circle reaches `radius + stroke width`.
    if has_ring {
        spec["stroke-align"] = Value::from("outside");
    }
    let fades_fill = opacity.is_some_and(|o| o != 1.0) || spec.get("opacity-expr").is_some();
    match (so, so_expr) {
        (Some(o), _) => spec["stroke-opacity"] = Value::from(o),
        (None, Some(e)) => spec["stroke-opacity-expr"] = e,
        (None, None) if has_ring && fades_fill => spec["stroke-opacity"] = Value::from(1.0),
        (None, None) => {}
    }

    // `circle-blur` → `blur` (constant) or `blur-expr`.
    let (blur, blur_expr) = resolve_number(paint.get("circle-blur"));
    if let Some(b) = blur {
        spec["blur"] = Value::from(b.max(0.0));
    }
    if let Some(e) = blur_expr {
        spec["blur-expr"] = e;
    }

    nodes.insert(circles_id.clone(), spec);
    outputs.push(circles_id);
}
