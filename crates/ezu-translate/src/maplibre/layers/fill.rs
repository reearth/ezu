//! `fill` and `fill-extrusion` layers → `fill-solid` (and `fill-pattern`
//! → a tiled sprite clipped to the polygon coverage).

use serde_json::{Map, Value};

use crate::maplibre::color::{parse_color, with_alpha};
use crate::maplibre::filter;
use crate::maplibre::layers::{paint_of, translated};
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
        if paint.get("fill-translate").is_some() {
            report.warn(format!(
                "layer `{id}`: `fill-translate` is not supported with `fill-pattern` — drawn unshifted"
            ));
        }
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

    // ezu always anti-aliases a fill's edges, and MapLibre draws
    // `fill-outline-color` only while `fill-antialias` is on.
    if paint.get("fill-antialias").and_then(Value::as_bool) == Some(false) {
        report.warn(format!(
            "layer `{id}`: `fill-antialias: false` is not supported — drawn anti-aliased"
        ));
    }
    let fill_color = paint.get("fill-color");
    // `fill-opacity` → constant `fill-alpha` (literal) else a raw expression
    // emitted as `opacity-expr`, evaluated per tile.
    let (opacity, opacity_expr) = resolve_number(paint.get("fill-opacity"));
    // `fill-outline-color` → a 1px outline: ezu `fill-solid` `edge` for a
    // literal colour, `edge-expr` for an expression.
    let (outline, outline_expr) = resolve_paint_color(paint.get("fill-outline-color"));
    if paint.get("fill-outline-color").is_some() && outline.is_none() && outline_expr.is_none() {
        report.warn(format!(
            "layer `{id}`: fill-outline-color is unsupported — drawn without an outline"
        ));
    }

    // Resolve `fill-color` into either a constant hex (literal) or a raw
    // expression emitted as `fill-expr`, with MapLibre's default as the
    // constant the node always needs.
    let (hex, fill_expr) = resolve_paint_color(fill_color);
    let hex = color_or_default(id, "fill-color", fill_color, hex, &fill_expr, report);

    let feat_id = format!("{id}__feat");
    let fill_id = format!("{id}__fill");
    nodes.insert(
        feat_id.clone(),
        features_node(&source, &source_layer, base_filter_expr, min_zoom, max_zoom),
    );
    let features = translated(
        id,
        "fill-translate",
        paint,
        format!("@{feat_id}"),
        nodes,
        report,
    );
    let mut spec = serde_json::json!({ "op": "fill-solid", "features": features, "fill": hex });
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
    if let Some(expr) = outline_expr {
        spec["edge-expr"] = expr;
        spec["edge-width"] = Value::from(1.0);
    }
    // MapLibre's `fill-opacity` fades the outline as well as the fill, while
    // the node's `fill-alpha` / `opacity-expr` fade the fill alone, so the
    // same opacity goes to the outline's own field.
    if spec.get("edge-width").is_some() {
        if let Some(a) = opacity {
            spec["edge-alpha"] = Value::from(a);
        }
        if let Some(expr) = spec.get("opacity-expr").cloned() {
            spec["edge-alpha-expr"] = expr;
        }
    }
    nodes.insert(fill_id.clone(), spec);
    outputs.push(fill_id);
}

/// The MapLibre style spec's default for `fill-color`, `fill-extrusion-color`,
/// `line-color`, `circle-color`, `circle-stroke-color` and `text-color`:
/// what MapLibre draws when the property is unset, and so the constant a
/// node falls back to for a feature its colour expression gives no colour.
pub(crate) const DEFAULT_PAINT_COLOR: &str = "#000000";

/// The constant colour a paint node needs for `prop`: its literal `hex`,
/// else MapLibre's default, which also backs an expression (`expr`) for
/// features it does not resolve for. A value that is neither a colour nor
/// an expression warns and draws the default.
pub(crate) fn color_or_default(
    id: &str,
    prop: &str,
    value: Option<&Value>,
    hex: Option<String>,
    expr: &Option<Value>,
    report: &mut Report,
) -> String {
    warn_unreadable_color(id, prop, value, &hex, expr, report);
    hex.unwrap_or_else(|| DEFAULT_PAINT_COLOR.to_string())
}

/// Warn when colour paint property `prop` is set but is neither a colour
/// (`hex`) nor an expression (`expr`), so the node draws its default.
pub(crate) fn warn_unreadable_color(
    id: &str,
    prop: &str,
    value: Option<&Value>,
    hex: &Option<String>,
    expr: &Option<Value>,
    report: &mut Report,
) {
    if value.is_some() && hex.is_none() && expr.is_none() {
        report.warn(format!(
            "layer `{id}`: `{prop}` could not be read — drawn black"
        ));
    }
}

/// Route a colour paint property into ezu paint. Returns
/// `(constant_hex, color_expr)`:
/// - a literal colour string → `(Some(hex), None)` (the constant field),
///   a translucent colour's alpha kept as `#rrggbbaa`: every node's opacity
///   multiplies the colour's own alpha, as MapLibre's does.
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
    if let Some((hex, alpha)) = const_color(v).and_then(|s| parse_color(&s)) {
        return (Some(with_alpha(&hex, alpha)), None);
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
    let hex = color_or_default(id, "fill-extrusion-color", color, hex, &fill_expr, report);

    let feat_id = format!("{id}__ext_feat");
    let fill_id = format!("{id}__ext_fill");
    nodes.insert(
        feat_id.clone(),
        features_node(&source, &source_layer, base_filter_expr, min_zoom, max_zoom),
    );
    let features = translated(
        id,
        "fill-extrusion-translate",
        paint,
        format!("@{feat_id}"),
        nodes,
        report,
    );
    let mut spec = serde_json::json!({ "op": "fill-solid", "features": features, "fill": hex });
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
/// `icon` → `tiling` as the pattern, composed with
/// `blend { composite: "source-atop" }` (which keeps the pattern only
/// inside the polygons).
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
            "over": format!("@{tile_id}"), "composite": "source-atop"
        }),
    );
    outputs.push(out_id);
}
