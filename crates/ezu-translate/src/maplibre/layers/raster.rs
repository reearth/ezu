//! `raster` layer → an ezu `raster` node bound to the named source, its
//! paint properties carried onto the node's colour fields and its
//! resampling onto the source.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::maplibre::layers::{paint_of, with_zoom_range};
use crate::maplibre::{const_number, is_expr, Report, ZoomRange};

/// Each MapLibre raster paint property the `raster` node carries, with
/// the node field it lands on and MapLibre's default, which is also the
/// node's: a property at its default is left off the node.
const COLOUR_FIELDS: &[(&str, &str, f64)] = &[
    ("raster-opacity", "opacity", 1.0),
    ("raster-hue-rotate", "hue-rotate-deg", 0.0),
    ("raster-saturation", "saturation", 0.0),
    ("raster-contrast", "contrast", 0.0),
    ("raster-brightness-min", "brightness-min", 0.0),
    ("raster-brightness-max", "brightness-max", 1.0),
];

/// Which resampling each raster source was given by the first layer that
/// read it, so a later layer asking for the other one gets a copy of the
/// source instead of changing what the first layer draws.
pub(crate) type ResamplingClaims = HashMap<String, &'static str>;

#[allow(clippy::too_many_arguments)]
pub(crate) fn convert_raster(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    zoom_range: ZoomRange,
    source_defs: &mut Map<String, Value>,
    claims: &mut ResamplingClaims,
    report: &mut Report,
) {
    // A raster layer references a raster source by name; ezu's `raster`
    // node picks it up by source name. We already emitted the source.
    let Some(src) = layer.get("source").and_then(Value::as_str) else {
        report.warn(format!("layer `{id}`: raster without source — skipped"));
        return;
    };
    let paint = paint_of(layer);
    let src = resampled_source(id, src, paint, source_defs, claims, report);
    let mut node = serde_json::json!({ "op": "raster", "source": src });
    for &(prop, field, default) in COLOUR_FIELDS {
        let Some(v) = paint.get(prop) else {
            continue;
        };
        // Raster paint may follow the zoom but never a feature (a raster
        // has none), so an expression is evaluated once per tile by an
        // `expr` node feeding the field.
        if is_expr(v) {
            let expr_id = format!("{id}__{field}");
            nodes.insert(
                expr_id.clone(),
                serde_json::json!({ "op": "expr", "type": "number", "expr": v }),
            );
            node[field] = Value::from(format!("@{expr_id}"));
            continue;
        }
        match const_number(v) {
            Some(n) if n == default => {}
            Some(n) => node[field] = Value::from(n),
            None => report.warn(format!(
                "layer `{id}`: `{prop}` could not be read as a number — left at its default"
            )),
        }
    }
    let nid = format!("{id}__raster");
    nodes.insert(nid.clone(), with_zoom_range(node, zoom_range));
    outputs.push(nid);
}

/// The source the layer's `raster` node reads, carrying the layer's
/// `raster-resampling`. ezu resamples a raster where the host stitches a
/// source's tiles, so the setting lives on the source; the first layer to
/// read a source sets it there, and a later layer that wants the other
/// one reads a copy of the source made for it.
fn resampled_source(
    id: &str,
    src: &str,
    paint: &Map<String, Value>,
    source_defs: &mut Map<String, Value>,
    claims: &mut ResamplingClaims,
    report: &mut Report,
) -> String {
    let wanted = match paint.get("raster-resampling") {
        None => "linear",
        Some(v) => match v.as_str() {
            Some("nearest") => "nearest",
            Some("linear") => "linear",
            _ => {
                report.warn(format!(
                    "layer `{id}`: `raster-resampling` carries over only as a constant \
                     `linear` or `nearest` — sampled linearly"
                ));
                "linear"
            }
        },
    };
    let Some(def) = source_defs.get(src).cloned() else {
        // The source was skipped (and warned about) already; the node
        // names it all the same and draws nothing.
        return src.to_string();
    };
    let claimed = *claims.entry(src.to_string()).or_insert(wanted);
    if claimed == wanted {
        if wanted == "nearest" {
            source_defs[src]["resampling"] = Value::from("nearest");
        }
        return src.to_string();
    }
    let copy = format!("{src}__{wanted}");
    if !source_defs.contains_key(&copy) {
        let mut def = def;
        match wanted {
            "nearest" => def["resampling"] = Value::from("nearest"),
            _ => {
                if let Some(o) = def.as_object_mut() {
                    o.remove("resampling");
                }
            }
        }
        source_defs.insert(copy.clone(), def);
    }
    copy
}
