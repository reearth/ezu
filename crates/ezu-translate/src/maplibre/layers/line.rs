//! `line` layer → a crisp `stroke` (or `line-stamp` when `line-pattern`
//! is set), over the layer's lines and polygon rings.

use serde_json::{Map, Value};

use crate::maplibre::filter;
use crate::maplibre::layers::fill::{resolve_number, resolve_paint_color};
use crate::maplibre::layers::{paint_of, translated};
use crate::maplibre::sources::{features_node, resolve_layer_source, Sources};
use crate::maplibre::{const_number, is_expr, Report, ZoomRange};

/// MapLibre's default `line-miter-limit`.
const MAPLIBRE_MITER_LIMIT: f64 = 2.0;

pub(crate) fn convert_line(
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
    let base_filter_expr = filter::layer_filter_expr(layer, report, id);
    let paint = paint_of(layer);

    // `line-width` → a constant `width-px` (literal) else a raw expression
    // emitted as `width-expr`, evaluated per tile, with a constant fallback
    // for a feature the expression gives no number.
    let (width_const, width_expr) = resolve_number(paint.get("line-width"));
    let width = width_const.unwrap_or(1.0).max(0.1);

    // `line-gap-width` → `gap-width-px` / `gap-width-expr`: a road casing,
    // drawn as two parallel bands of `line-width` around a gap of this size.
    let (gap_const, gap_expr) = resolve_number(paint.get("line-gap-width"));

    // `line-pattern` replaces the solid stroke: repeat the named sprite icon
    // along each line, scaled to the stroke width (`line-stamp`).
    if let Some(pattern) = paint.get("line-pattern") {
        if paint.get("line-gap-width").is_some() {
            report.warn(format!(
                "layer `{id}`: `line-gap-width` is not supported with `line-pattern` — the pattern is drawn along the centreline"
            ));
        }
        for prop in ["line-translate", "line-offset"] {
            if paint.get(prop).is_some() {
                report.warn(format!(
                    "layer `{id}`: `{prop}` is not supported with `line-pattern` — drawn unshifted"
                ));
            }
        }
        let (opacity, opacity_expr) = resolve_number(paint.get("line-opacity"));
        let paint = PatternPaint {
            width,
            width_expr,
            opacity,
            opacity_expr,
        };
        convert_line_pattern(
            id,
            pattern,
            &source,
            &source_layer,
            base_filter_expr,
            zoom_range,
            paint,
            sources,
            nodes,
            outputs,
            report,
        );
        return;
    }

    // `line-color` → constant `color` (literal) else a raw expression emitted
    // as `color-expr` (with `#000000` as the constant fallback the `stroke`
    // node always needs).
    let (color_hex, color_expr) = resolve_paint_color(paint.get("line-color"));
    let hex = color_hex.unwrap_or_else(|| "#000000".to_string());
    // `line-opacity` → constant `opacity` (literal) else a raw expression
    // emitted as `opacity-expr`.
    let (opacity, opacity_expr) = resolve_number(paint.get("line-opacity"));

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

    let stroke_id = format!("{id}__stroke");
    let lines_id = line_features(
        id,
        &source,
        &source_layer,
        base_filter_expr,
        zoom_range,
        nodes,
    );
    let features = translated(
        id,
        "line-translate",
        paint,
        format!("@{lines_id}"),
        nodes,
        report,
    );
    // Crisp `stroke` (tiny-skia) rather than a painterly brush, to match
    // MapLibre's clean vector lines.
    let mut spec = serde_json::json!({
        "op": "stroke",
        "features": features,
        "color": hex,
        "width-px": width,
        "cap": cap,
        "join": join,
    });
    if let Some(expr) = color_expr {
        spec["color-expr"] = expr;
    }
    if let Some(expr) = width_expr {
        spec["width-expr"] = expr;
    }
    // `layout.line-miter-limit` → `miter-limit`. MapLibre bevels a miter
    // join past 2 half-widths unless told otherwise, where `stroke` keeps
    // tiny-skia's 4, so a miter-joined line always carries the limit. A
    // zoom curve becomes an `expr` node feeding the field; other joins
    // never reach it.
    if join == "miter" {
        spec["miter-limit"] = match layout.and_then(|l| l.get("line-miter-limit")) {
            Some(v) if is_expr(v) => {
                let expr_id = format!("{id}__miter_limit");
                nodes.insert(
                    expr_id.clone(),
                    serde_json::json!({ "op": "expr", "type": "number", "expr": v }),
                );
                Value::from(format!("@{expr_id}"))
            }
            Some(v) => match const_number(v) {
                Some(n) => Value::from(n),
                None => {
                    report.warn(format!(
                        "layer `{id}`: `line-miter-limit` could not be read as a number — using MapLibre's default 2"
                    ));
                    Value::from(MAPLIBRE_MITER_LIMIT)
                }
            },
            None => Value::from(MAPLIBRE_MITER_LIMIT),
        };
    }
    if let Some(g) = gap_const {
        spec["gap-width-px"] = Value::from(g);
    }
    if let Some(expr) = gap_expr {
        spec["gap-width-expr"] = expr;
    }
    // `line-offset` → `offset-px` / `offset-expr`: the line drawn beside
    // itself, to the right of its direction of travel.
    let (offset, offset_expr) = resolve_number(paint.get("line-offset"));
    if let Some(o) = offset.filter(|o| *o != 0.0) {
        spec["offset-px"] = Value::from(o);
    }
    if let Some(expr) = offset_expr {
        spec["offset-expr"] = expr;
    }
    if let Some(a) = opacity {
        spec["opacity"] = Value::from(a);
    }
    if let Some(expr) = opacity_expr {
        spec["opacity-expr"] = expr;
    }
    // MapLibre `line-dasharray` is in units of line width, so the lengths
    // carry over as they are and `dash-unit: width` scales them by the
    // stroke's width — the width each feature resolves `width-expr` to, when
    // `line-width` is an expression.
    if let Some(v) = paint.get("line-dasharray") {
        match const_dasharray(v) {
            Some(dash) if !dash.is_empty() => {
                spec["dasharray"] = Value::Array(dash.into_iter().map(Value::from).collect());
                spec["dash-unit"] = Value::from("width");
            }
            Some(_) => {}
            None => report.warn(format!(
                "layer `{id}`: `line-dasharray` carries over only as a constant array — drawn solid"
            )),
        }
    }
    nodes.insert(stroke_id.clone(), spec);
    outputs.push(stroke_id);
}

/// A constant `line-dasharray`: a bare array of numbers, or one wrapped in
/// `["literal", …]`. `None` for an expression or anything else that is
/// not a list of non-negative numbers.
fn const_dasharray(v: &Value) -> Option<Vec<f64>> {
    let arr = match v.as_array()?.as_slice() {
        [Value::String(op), Value::Array(inner)] if op == "literal" => inner,
        _ => v.as_array()?,
    };
    arr.iter()
        .map(|d| d.as_f64().filter(|d| *d >= 0.0))
        .collect()
}

/// The features a line layer draws, as polylines: `features` → `boundary`.
/// Returns the id of the node to stroke.
///
/// A MapLibre line layer on a polygon source-layer strokes the polygons'
/// rings, while `stroke` and `line-stamp` only draw polylines, so the rings
/// are turned into polylines first. `clip-edges: drop` leaves out the edges
/// the tile encoder added where it clipped a polygon at the tile buffer —
/// MapLibre skips those too, or every tile seam would carry a line. Line
/// geometry passes `boundary` unchanged, so the node is always inserted.
fn line_features(
    id: &str,
    source: &str,
    source_layer: &str,
    filter_expr: Option<Value>,
    (min_zoom, max_zoom): ZoomRange,
    nodes: &mut Map<String, Value>,
) -> String {
    let feat_id = format!("{id}__feat");
    nodes.insert(
        feat_id.clone(),
        features_node(source, source_layer, filter_expr, min_zoom, max_zoom),
    );
    let lines_id = format!("{id}__lines");
    nodes.insert(
        lines_id.clone(),
        serde_json::json!({
            "op": "boundary",
            "features": format!("@{feat_id}"),
            "clip-edges": "drop",
        }),
    );
    lines_id
}

/// The paint a `line-pattern` layer carries over to `line-stamp`: the
/// constant width (the fallback the node needs), and each of `line-width` /
/// `line-opacity` as a constant or a raw expression.
pub(crate) struct PatternPaint {
    pub(crate) width: f64,
    pub(crate) width_expr: Option<Value>,
    pub(crate) opacity: Option<f64>,
    pub(crate) opacity_expr: Option<Value>,
}

/// `line-pattern` → repeat the named sprite icon along each line, fit to the
/// stroke width: `features` → `icon` → `line-stamp`. A data-driven pattern
/// names each feature's icon instead, through `line-stamp`'s `name-expr` over
/// the style's sprite sheet.
#[allow(clippy::too_many_arguments)]
pub(crate) fn convert_line_pattern(
    id: &str,
    pattern: &Value,
    source: &str,
    source_layer: &str,
    base_filter_expr: Option<Value>,
    zoom_range: ZoomRange,
    paint: PatternPaint,
    sources: &Sources,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    report: &mut Report,
) {
    // A constant pattern crops its icon up front (`icon` node → `image`); a
    // data-driven one crops each feature's icon at eval time.
    let constant = match pattern.as_str() {
        Some(name) => {
            let Some(icon) = sources.resolve_icon(name) else {
                report.warn(format!(
                    "layer `{id}`: line-pattern `{name}` needs a `sprite`, but the style declares none — skipped"
                ));
                return;
            };
            Some(icon)
        }
        None => None,
    };
    let sprite_src = match constant {
        Some((sprite_src, _)) => sprite_src,
        None => {
            let Some(sprite_src) = sources.default_sprite() else {
                report.warn(format!(
                    "layer `{id}`: data-driven `line-pattern` needs a `sprite`, but the style declares none — skipped"
                ));
                return;
            };
            sprite_src
        }
    };
    let lines_id = line_features(
        id,
        source,
        source_layer,
        base_filter_expr,
        zoom_range,
        nodes,
    );
    let out_id = format!("{id}__linepat");
    let mut spec = serde_json::json!({
        "op": "line-stamp", "features": format!("@{lines_id}"),
    });
    match constant {
        Some((_, icon_name)) => {
            let icon_id = format!("{id}__icon");
            nodes.insert(
                icon_id.clone(),
                serde_json::json!({ "op": "icon", "sprite": format!("@{sprite_src}"), "name": icon_name }),
            );
            spec["image"] = Value::from(format!("@{icon_id}"));
        }
        None => {
            spec["sprite"] = Value::from(format!("@{sprite_src}"));
            spec["name-expr"] = pattern.clone();
        }
    }
    spec["width-px"] = Value::from(paint.width);
    if let Some(expr) = paint.width_expr {
        spec["width-expr"] = expr;
    }
    if let Some(a) = paint.opacity {
        spec["opacity"] = Value::from(a);
    }
    if let Some(expr) = paint.opacity_expr {
        spec["opacity-expr"] = expr;
    }
    nodes.insert(out_id.clone(), spec);
    outputs.push(out_id);
}
