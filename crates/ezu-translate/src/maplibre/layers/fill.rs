//! `fill` and `fill-extrusion` layers → `fill-solid` (and `fill-pattern`
//! → a tiled sprite clipped to the polygon coverage).

use serde_json::{Map, Value};

use crate::maplibre::color::parse_color;
use crate::maplibre::filter;
use crate::maplibre::layers::paint_of;
use crate::maplibre::sources::{features_node, resolve_layer_source, Sources};
use crate::maplibre::{const_color, const_number, is_expr};
use crate::maplibre::{Report, ZoomRange};

pub(crate) fn convert_fill(
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

    // `fill-pattern` takes precedence over `fill-color`: tile the named
    // sprite icon across the canvas and clip it to the polygon shape.
    if let Some(pattern) = paint.get("fill-pattern") {
        convert_fill_pattern(
            id,
            pattern,
            &source,
            &source_layer,
            base_filter_expr,
            zoom_range,
            sources,
            nodes,
            outputs,
            report,
        );
        return;
    }

    let fill_color = paint.get("fill-color");
    // `fill-opacity` → constant `fill-alpha` (literal) else a raw expression
    // emitted as `opacity-expr`, evaluated per tile.
    let (opacity, opacity_expr) = resolve_number(paint.get("fill-opacity"));
    // `fill-outline-color` → a 1px outline (ezu `fill-solid` `edge`). Only a
    // literal colour is supported; an expression outline is dropped.
    let outline: Option<String> = paint
        .get("fill-outline-color")
        .and_then(const_color)
        .and_then(|v| parse_color(&v))
        .map(|(hex, _)| hex);

    // Resolve `fill-color` into either a constant hex (literal) or a raw
    // expression emitted as `fill-expr`.
    let (hex, fill_expr) = resolve_paint_color(fill_color);
    if fill_expr.is_none() && hex.is_none() {
        report.warn(format!(
            "layer `{id}`: fill-color is data-driven/unsupported — using grey fallback"
        ));
    }
    // The node always needs a valid constant `fill` (a fallback color used
    // when the expression doesn't resolve for a group).
    let hex = hex.unwrap_or_else(|| "#808080".to_string());

    let feat_id = format!("{id}__feat");
    let fill_id = format!("{id}__fill");
    nodes.insert(
        feat_id.clone(),
        features_node(&source, &source_layer, base_filter_expr, min_zoom, max_zoom),
    );
    let mut spec =
        serde_json::json!({ "op": "fill-solid", "features": format!("@{feat_id}"), "fill": hex });
    if let Some(expr) = fill_expr {
        spec["fill-expr"] = expr;
    }
    if let Some(a) = opacity {
        spec["fill-alpha"] = Value::from(a);
    }
    if let Some(expr) = opacity_expr {
        spec["opacity-expr"] = expr;
    }
    if let Some(edge) = &outline {
        spec["edge"] = Value::from(edge.clone());
        spec["edge-width"] = Value::from(1.0);
    }
    nodes.insert(fill_id.clone(), spec);
    outputs.push(fill_id);
}

/// Route a colour paint property into ezu paint. Returns
/// `(constant_hex, color_expr)`:
/// - a literal colour string → `(Some(hex), None)` (the constant field).
/// - any function/expression (array or legacy object) → `(None, Some(raw))`
///   so it can be emitted as an `*-expr` field, evaluated per tile.
/// - a bare unsupported value → `(None, None)`.
pub(crate) fn resolve_paint_color(value: Option<&Value>) -> (Option<String>, Option<Value>) {
    let Some(v) = value else {
        return (None, None);
    };
    if is_expr(v) {
        return (None, Some(v.clone()));
    }
    if let Some((hex, _)) = const_color(v).and_then(|s| parse_color(&s)) {
        return (Some(hex), None);
    }
    (None, None)
}

/// Route a numeric paint property into ezu paint. Returns
/// `(constant, number_expr)`:
/// - a literal number → `(Some(n), None)`.
/// - any function/expression (array or legacy object) → `(None, Some(raw))`.
/// - otherwise → `(None, None)`.
pub(crate) fn resolve_number(value: Option<&Value>) -> (Option<f64>, Option<Value>) {
    let Some(v) = value else {
        return (None, None);
    };
    if is_expr(v) {
        return (None, Some(v.clone()));
    }
    if let Some(n) = const_number(v) {
        return (Some(n), None);
    }
    (None, None)
}

/// `fill-extrusion` → a plain 2-D footprint fill. ezu is a top-down CPU
/// raster renderer with no 3-D camera, so the extrusion (height / base) is
/// dropped; the polygons are filled with `fill-extrusion-color`. Any
/// stylized fake-3-D (height shading, offset shadows) is left to ezu-side
/// node composition rather than baked into the converter.
pub(crate) fn convert_fill_extrusion(
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
    let color = paint.get("fill-extrusion-color");
    // `fill-extrusion-opacity` → constant `fill-alpha` (literal) else a raw
    // expression emitted as `opacity-expr`.
    let (opacity, opacity_expr) = resolve_number(paint.get("fill-extrusion-opacity"));

    // `fill-extrusion-color` → constant `fill` (literal) else a raw
    // expression emitted as `fill-expr`.
    let (hex, fill_expr) = resolve_paint_color(color);
    if fill_expr.is_none() && hex.is_none() {
        report.warn(format!(
            "layer `{id}`: fill-extrusion-color is data-driven/unsupported — using grey fallback"
        ));
    }
    let hex = hex.unwrap_or_else(|| "#808080".to_string());

    let feat_id = format!("{id}__ext_feat");
    let fill_id = format!("{id}__ext_fill");
    nodes.insert(
        feat_id.clone(),
        features_node(&source, &source_layer, base_filter_expr, min_zoom, max_zoom),
    );
    let mut spec =
        serde_json::json!({ "op": "fill-solid", "features": format!("@{feat_id}"), "fill": hex });
    if let Some(expr) = fill_expr {
        spec["fill-expr"] = expr;
    }
    if let Some(a) = opacity {
        spec["fill-alpha"] = Value::from(a);
    }
    if let Some(expr) = opacity_expr {
        spec["opacity-expr"] = expr;
    }
    nodes.insert(fill_id.clone(), spec);
    outputs.push(fill_id);
}

/// `fill-pattern` → tile the named sprite icon across the canvas and clip it
/// to the polygon coverage: `fill-solid` (opaque shape) as the clip base,
/// `icon` → `tiling` as the pattern, composed with `blend { clip: true }`
/// (source-atop keeps the pattern only inside the polygons).
#[allow(clippy::too_many_arguments)]
pub(crate) fn convert_fill_pattern(
    id: &str,
    pattern: &Value,
    source: &str,
    source_layer: &str,
    base_filter_expr: Option<Value>,
    zoom_range: ZoomRange,
    sources: &Sources,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    report: &mut Report,
) {
    let (min_zoom, max_zoom) = zoom_range;
    let Some(name) = pattern.as_str() else {
        report.warn(format!(
            "layer `{id}`: data-driven `fill-pattern` not supported — skipped"
        ));
        return;
    };
    let Some((sprite_src, icon_name)) = sources.resolve_icon(name) else {
        report.warn(format!(
            "layer `{id}`: fill-pattern `{name}` needs a `sprite`, but the style declares none — skipped"
        ));
        return;
    };
    let feat_id = format!("{id}__feat");
    nodes.insert(
        feat_id.clone(),
        features_node(source, source_layer, base_filter_expr, min_zoom, max_zoom),
    );
    let shape_id = format!("{id}__shape");
    nodes.insert(
        shape_id.clone(),
        serde_json::json!({ "op": "fill-solid", "features": format!("@{feat_id}"), "fill": "#ffffff" }),
    );
    let icon_id = format!("{id}__icon");
    nodes.insert(
        icon_id.clone(),
        serde_json::json!({ "op": "icon", "sprite": format!("@{sprite_src}"), "name": icon_name }),
    );
    let tile_id = format!("{id}__pattern");
    nodes.insert(
        tile_id.clone(),
        serde_json::json!({ "op": "tiling", "input": format!("@{icon_id}"), "anchor": "world" }),
    );
    let out_id = format!("{id}__patfill");
    nodes.insert(
        out_id.clone(),
        serde_json::json!({
            "op": "blend", "base": format!("@{shape_id}"),
            "over": format!("@{tile_id}"), "clip": true
        }),
    );
    outputs.push(out_id);
}
