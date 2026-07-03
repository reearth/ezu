//! `symbol` layer icon (`layout.icon-image`) → a sprite `stamp`ed at each
//! point feature. Text labels are not supported yet.

use serde_json::{Map, Value};

use crate::maplibre::filter;
use crate::maplibre::layers::fill::resolve_number;
use crate::maplibre::layers::paint_of;
use crate::maplibre::sources::{features_node, resolve_layer_source, Sources};
use crate::maplibre::{Report, ZoomRange};

/// A `symbol` layer's **icon** (`layout.icon-image`): place the named sprite
/// at each point feature (`features` → `icon` → `stamp`). Text labels
/// (`text-field`) are not supported yet — an icon+text layer draws the icon
/// and warns about the dropped text.
pub(crate) fn convert_symbol(
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
    let layout = layer.get("layout").and_then(Value::as_object);
    let icon_image = layout.and_then(|l| l.get("icon-image"));
    let has_text = layout
        .and_then(|l| l.get("text-field"))
        .is_some_and(|v| !v.is_null());

    let Some(icon_name) = icon_image.and_then(Value::as_str) else {
        if icon_image.is_some() {
            report.warn(format!(
                "layer `{id}`: data-driven `icon-image` not supported — skipped"
            ));
        } else if has_text {
            report.warn(format!(
                "layer `{id}`: text-only `symbol` (labels) not supported yet — skipped"
            ));
        } else {
            report.warn(format!(
                "layer `{id}`: `symbol` without a constant `icon-image` — skipped"
            ));
        }
        return;
    };
    let Some((sprite_src, sprite_icon)) = sources.resolve_icon(icon_name) else {
        report.warn(format!(
            "layer `{id}`: icon `{icon_name}` needs a `sprite`, but the style declares none — skipped"
        ));
        return;
    };
    if has_text {
        report.warn(format!(
            "layer `{id}`: drawing icon only — `text-field` labels not supported yet"
        ));
    }

    let base_filter_expr = filter::layer_filter_expr(layer, report, id);
    let feat_id = format!("{id}__feat");
    nodes.insert(
        feat_id.clone(),
        features_node(&source, &source_layer, base_filter_expr, min_zoom, max_zoom),
    );
    let icon_id = format!("{id}__icon");
    nodes.insert(
        icon_id.clone(),
        serde_json::json!({ "op": "icon", "sprite": format!("@{sprite_src}"), "name": sprite_icon }),
    );

    // `stamp` scale / rotation / opacity each carry a constant literal on the
    // plain field, or a data-driven expression / legacy `{stops}` object on
    // the matching `*-expr` sibling.
    let stamp_id = format!("{id}__stamp");
    let mut spec = serde_json::json!({
        "op": "stamp", "features": format!("@{feat_id}"), "image": format!("@{icon_id}")
    });

    // `layout.icon-size` → `scale` (constant) or `scale-expr`.
    let (size, size_expr) = resolve_number(layout.and_then(|l| l.get("icon-size")));
    if let Some(s) = size {
        if s != 1.0 {
            spec["scale"] = Value::from(s);
        }
    }
    if let Some(e) = size_expr {
        spec["scale-expr"] = e;
    }

    // `layout.icon-rotate` → `rotation-deg` (constant) or `rotation-deg-expr`.
    let (rotate, rotate_expr) = resolve_number(layout.and_then(|l| l.get("icon-rotate")));
    if let Some(r) = rotate {
        if r != 0.0 {
            spec["rotation-deg"] = Value::from(r);
        }
    }
    if let Some(e) = rotate_expr {
        spec["rotation-deg-expr"] = e;
    }

    // `paint.icon-opacity` → `opacity` (constant) or `opacity-expr`.
    let (opacity, opacity_expr) = resolve_number(paint_of(layer).get("icon-opacity"));
    if let Some(a) = opacity {
        spec["opacity"] = Value::from(a);
    }
    if let Some(e) = opacity_expr {
        spec["opacity-expr"] = e;
    }

    nodes.insert(stamp_id.clone(), spec);
    outputs.push(stamp_id);
}
