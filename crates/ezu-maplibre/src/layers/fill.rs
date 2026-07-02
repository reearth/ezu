//! `fill` and `fill-extrusion` layers → `fill-solid` (and `fill-pattern`
//! → a tiled sprite clipped to the polygon coverage).

use serde_json::{Map, Value};

use crate::color::{self, parse_color};
use crate::filter;
use crate::layers::paint_of;
use crate::sources::{features_node, resolve_layer_source, Sources};
use crate::zoom;
use crate::{ConvertOptions, Report};

pub(crate) fn convert_fill(
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

    // `fill-pattern` takes precedence over `fill-color`: tile the named
    // sprite icon across the canvas and clip it to the polygon shape.
    if let Some(pattern) = paint.get("fill-pattern") {
        convert_fill_pattern(
            id,
            pattern,
            &source,
            &source_layer,
            base_filter,
            base_filter_expr,
            sources,
            nodes,
            outputs,
            report,
        );
        return;
    }

    let fill_color = paint.get("fill-color");
    let opacity = paint
        .get("fill-opacity")
        .and_then(|v| zoom::number_at(v, opts.zoom));
    // `fill-outline-color` → a 1px outline (ezu `fill-solid` `edge`).
    let outline: Option<String> = paint
        .get("fill-outline-color")
        .and_then(|v| zoom::color_at(v, opts.zoom))
        .and_then(|v| parse_color(&v))
        .map(|(hex, _)| hex);

    // `fill-color: ["match", ["get", prop], vals, color, ..., fallback]`
    // → one filtered fill-solid per bucket, plus a fallback underneath.
    if let Some(buckets) = fill_color.and_then(color::match_buckets) {
        // Fallback first (drawn underneath the specific buckets).
        let mut emit = |suffix: &str, filt: Option<Map<String, Value>>, hex: String| {
            let feat_id = format!("{id}__{suffix}_feat");
            let fill_id = format!("{id}__{suffix}_fill");
            nodes.insert(
                feat_id.clone(),
                features_node(&source, &source_layer, filt, base_filter_expr.clone()),
            );
            let mut spec = serde_json::json!({ "op": "fill-solid", "features": format!("@{feat_id}"), "fill": hex });
            if let Some(a) = opacity {
                spec["fill-alpha"] = Value::from(a);
            }
            if let Some(edge) = &outline {
                spec["edge"] = Value::from(edge.clone());
                spec["edge-width"] = Value::from(1.0);
            }
            nodes.insert(fill_id.clone(), spec);
            outputs.push(fill_id);
        };
        if let Some((hex, _)) = parse_color(&buckets.fallback) {
            emit("fallback", base_filter.clone(), hex);
        }
        for (i, (values, col)) in buckets.arms.iter().enumerate() {
            let Some((hex, _)) = parse_color(col) else {
                continue;
            };
            let mut filt = base_filter.clone().unwrap_or_default();
            filt.insert(buckets.key.clone(), values.clone());
            emit(&format!("b{i}"), Some(filt), hex);
        }
        return;
    }

    // Plain colour (possibly zoom-dependent).
    let (hex, _a) = fill_color
        .and_then(|v| zoom::color_at(v, opts.zoom))
        .and_then(|v| parse_color(&v))
        .unwrap_or_else(|| {
            report.warn(format!(
                "layer `{id}`: fill-color is data-driven/unsupported — using grey fallback"
            ));
            ("#808080".to_string(), 1.0)
        });
    let feat_id = format!("{id}__feat");
    let fill_id = format!("{id}__fill");
    nodes.insert(
        feat_id.clone(),
        features_node(&source, &source_layer, base_filter, base_filter_expr),
    );
    let mut spec =
        serde_json::json!({ "op": "fill-solid", "features": format!("@{feat_id}"), "fill": hex });
    if let Some(a) = opacity {
        spec["fill-alpha"] = Value::from(a);
    }
    if let Some(edge) = &outline {
        spec["edge"] = Value::from(edge.clone());
        spec["edge-width"] = Value::from(1.0);
    }
    nodes.insert(fill_id.clone(), spec);
    outputs.push(fill_id);
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
    opts: &ConvertOptions,
    sources: &Sources,
    report: &mut Report,
) {
    let Some((source, source_layer)) = resolve_layer_source(id, layer, sources, report) else {
        return;
    };
    let (base_filter, base_filter_expr) = filter::layer_filters(layer, report, id);
    let paint = paint_of(layer);
    let color = paint.get("fill-extrusion-color");
    let opacity = paint
        .get("fill-extrusion-opacity")
        .and_then(|v| zoom::number_at(v, opts.zoom));

    let mut emit = |suffix: &str, filt: Option<Map<String, Value>>, hex: String| {
        let feat_id = format!("{id}__{suffix}_feat");
        let fill_id = format!("{id}__{suffix}_fill");
        nodes.insert(
            feat_id.clone(),
            features_node(&source, &source_layer, filt, base_filter_expr.clone()),
        );
        let mut spec = serde_json::json!({ "op": "fill-solid", "features": format!("@{feat_id}"), "fill": hex });
        if let Some(a) = opacity {
            spec["fill-alpha"] = Value::from(a);
        }
        nodes.insert(fill_id.clone(), spec);
        outputs.push(fill_id);
    };

    // `fill-extrusion-color` may be a `match` on a property, like fill-color.
    if let Some(buckets) = color.and_then(color::match_buckets) {
        if let Some((hex, _)) = parse_color(&buckets.fallback) {
            emit("fallback", base_filter.clone(), hex);
        }
        for (i, (values, col)) in buckets.arms.iter().enumerate() {
            let Some((hex, _)) = parse_color(col) else {
                continue;
            };
            let mut filt = base_filter.clone().unwrap_or_default();
            filt.insert(buckets.key.clone(), values.clone());
            emit(&format!("b{i}"), Some(filt), hex);
        }
        return;
    }

    let (hex, _a) = color
        .and_then(|v| zoom::color_at(v, opts.zoom))
        .and_then(|v| parse_color(&v))
        .unwrap_or_else(|| {
            report.warn(format!(
                "layer `{id}`: fill-extrusion-color is data-driven/unsupported — using grey fallback"
            ));
            ("#808080".to_string(), 1.0)
        });
    emit("ext", base_filter, hex);
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
    base_filter: Option<Map<String, Value>>,
    base_filter_expr: Option<Value>,
    sources: &Sources,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    report: &mut Report,
) {
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
        features_node(source, source_layer, base_filter, base_filter_expr),
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
