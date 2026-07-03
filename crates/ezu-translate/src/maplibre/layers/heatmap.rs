//! `heatmap` layer → `density` (kernel-density field) + `color-ramp`
//! (the layer's `heatmap-color` as a raw `ramp-expr`).

use serde_json::{Map, Value};

use crate::maplibre::filter;
use crate::maplibre::layers::fill::resolve_number;
use crate::maplibre::layers::paint_of;
use crate::maplibre::sources::{features_node, resolve_layer_source, Sources};
use crate::maplibre::{Report, ZoomRange};

/// Fallback pad bound (px) for a `heatmap-radius` expression whose maximum
/// can't be derived from its literals (e.g. a data-driven `["get", ...]`).
/// The `density` node clamps evaluated radii to it.
const RADIUS_BOUND_CAP: f64 = 100.0;

/// The MapLibre style spec's default `heatmap-color` ramp. `royalblue` is
/// hex-encoded because maplibre-expr's named-colour table only carries the
/// basic CSS names.
fn default_heatmap_color() -> Value {
    serde_json::json!([
        "interpolate",
        ["linear"],
        ["heatmap-density"],
        0,
        "rgba(0, 0, 255, 0)",
        0.1,
        "#4169e1",
        0.3,
        "cyan",
        0.5,
        "lime",
        0.7,
        "yellow",
        1,
        "red"
    ])
}

/// A `heatmap` layer → `features` → `density` → `color-ramp`, wired into the
/// blend chain like any other layer. `heatmap-radius` / `heatmap-weight` /
/// `heatmap-intensity` route constant-vs-expression onto the `density` node;
/// `heatmap-color` passes through raw as the ramp's `ramp-expr` (the spec's
/// default ramp when absent).
pub(crate) fn convert_heatmap(
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

    let dens_id = format!("{id}__density");
    let mut spec = serde_json::json!({
        "op": "density",
        "features": format!("@{feat_id}"),
    });

    // `heatmap-radius` → `radius` (constant) or `radius-expr`. Pad is
    // computed at build time from the constant `radius`, so an
    // expression-only radius still needs one: the expression's own
    // maximum when its outputs are plain literals, else a capped default.
    let (radius, radius_expr) = resolve_number(paint.get("heatmap-radius"));
    if let Some(r) = radius {
        spec["radius"] = Value::from(r.max(0.0));
    }
    if let Some(e) = radius_expr {
        let bound = radius_bound_from_expr(&e).unwrap_or_else(|| {
            report.warn(format!(
                "layer `{id}`: heatmap-radius expression has no derivable maximum — \
                 capping the kernel radius at {RADIUS_BOUND_CAP}px"
            ));
            RADIUS_BOUND_CAP
        });
        spec["radius"] = Value::from(bound);
        spec["radius-expr"] = e;
    }

    // `heatmap-weight` → `weight-expr`. The node has no constant weight
    // field; a constant becomes a literal expression (a bare number is a
    // valid MapLibre expression).
    let (weight, weight_expr) = resolve_number(paint.get("heatmap-weight"));
    if let Some(w) = weight {
        spec["weight-expr"] = Value::from(w.max(0.0));
    }
    if let Some(e) = weight_expr {
        spec["weight-expr"] = e;
    }

    // `heatmap-intensity` → `intensity` (constant) or `intensity-expr`.
    let (intensity, intensity_expr) = resolve_number(paint.get("heatmap-intensity"));
    if let Some(i) = intensity {
        spec["intensity"] = Value::from(i.max(0.0));
    }
    if let Some(e) = intensity_expr {
        spec["intensity-expr"] = e;
    }
    nodes.insert(dens_id.clone(), spec);

    // `heatmap-color` → the ramp's `ramp-expr`, raw. The property is an
    // expression by definition; anything else falls back to the default.
    let ramp = match paint.get("heatmap-color") {
        Some(v) if v.is_array() => v.clone(),
        _ => default_heatmap_color(),
    };
    let ramp_id = format!("{id}__ramp");
    let mut ramp_spec = serde_json::json!({
        "op": "color-ramp",
        "field": format!("@{dens_id}"),
        "ramp-expr": ramp,
    });

    // `heatmap-opacity` → the ramp's `opacity`. A zoom curve (the common
    // heatmap→circle crossfade) goes through an `expr` scalar node wired
    // into the field's port.
    let (opacity, opacity_expr) = resolve_number(paint.get("heatmap-opacity"));
    if let Some(o) = opacity {
        if o != 1.0 {
            ramp_spec["opacity"] = Value::from(o.clamp(0.0, 1.0));
        }
    }
    if let Some(e) = opacity_expr {
        let op_id = format!("{id}__opacity");
        nodes.insert(
            op_id.clone(),
            serde_json::json!({ "op": "expr", "expr": e }),
        );
        ramp_spec["opacity"] = Value::from(format!("@{op_id}"));
    }
    nodes.insert(ramp_id.clone(), ramp_spec);
    outputs.push(ramp_id);
}

/// Derive a safe constant upper bound for a `heatmap-radius` expression.
/// For `interpolate`/`step` (and the legacy `{stops}` object) whose outputs
/// are all plain numeric literals, every possible output lies between the
/// smallest and largest of them — so the max literal output is a sound
/// (over-)estimate for the pad bound. Anything else returns `None`.
fn radius_bound_from_expr(v: &Value) -> Option<f64> {
    // Legacy function object: `{ "stops": [[in, out], ...], ... }`.
    if let Some(obj) = v.as_object() {
        let stops = obj.get("stops")?.as_array()?;
        return max_of(stops.iter().map(|s| s.get(1)));
    }
    let arr = v.as_array()?;
    match arr.first()?.as_str()? {
        // ["interpolate", [..], input, in1, out1, in2, out2, ...]
        "interpolate" | "interpolate-hcl" | "interpolate-lab" => {
            max_of(arr.get(3..)?.iter().skip(1).step_by(2).map(Some))
        }
        // ["step", input, out0, in1, out1, in2, out2, ...]
        "step" => max_of(arr.get(2..)?.iter().step_by(2).map(Some)),
        _ => None,
    }
}

/// Max of the given values; `None` unless every one is a plain number.
fn max_of<'a>(outputs: impl Iterator<Item = Option<&'a Value>>) -> Option<f64> {
    let mut max: Option<f64> = None;
    for out in outputs {
        let n = out?.as_f64()?;
        max = Some(max.map_or(n, |m| m.max(n)));
    }
    max
}
