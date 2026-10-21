//! Per-layer-type converters. Each MapLibre layer `type` lowers to a small
//! subgraph of ezu nodes; the modules here hold one concern each and the
//! dispatch loop in [`crate::maplibre::convert`] calls into them.

use serde_json::{Map, Value};

pub(crate) mod background;
pub(crate) mod circle;
pub(crate) mod fill;
pub(crate) mod heatmap;
pub(crate) mod hillshade;
pub(crate) mod line;
pub(crate) mod raster;
pub(crate) mod symbol;

pub(crate) use background::convert_background;
pub(crate) use circle::convert_circle;
pub(crate) use fill::{convert_fill, convert_fill_extrusion};
pub(crate) use heatmap::convert_heatmap;
pub(crate) use hillshade::convert_hillshade;
pub(crate) use line::convert_line;
pub(crate) use raster::{convert_raster, ResamplingClaims};
pub(crate) use symbol::{convert_symbol, emit_label_placement};

/// Put a layer's zoom band on a node that gates on `min-zoom`/`max-zoom`
/// itself (`raster`, `dem`), leaving out a bound the layer does not set.
pub(crate) fn with_zoom_range(mut node: Value, (min, max): crate::maplibre::ZoomRange) -> Value {
    if let Some(z) = min {
        node["min-zoom"] = Value::from(z);
    }
    if let Some(z) = max {
        node["max-zoom"] = Value::from(z);
    }
    node
}

/// The paint and layout properties a layer type's converter reads, carrying
/// each over or warning itself where it cannot, by MapLibre layer `type`.
/// `visibility` is read by the layer walk for every type.
fn read_properties(ty: &str) -> &'static [&'static str] {
    match ty {
        "background" => &[
            "background-color",
            "background-opacity",
            "background-pattern",
        ],
        "fill" => &[
            "fill-color",
            "fill-opacity",
            "fill-outline-color",
            "fill-pattern",
            "fill-translate",
            "fill-antialias",
        ],
        "fill-extrusion" => &[
            "fill-extrusion-color",
            "fill-extrusion-opacity",
            "fill-extrusion-translate",
        ],
        "line" => &[
            "line-color",
            "line-opacity",
            "line-width",
            "line-gap-width",
            "line-dasharray",
            "line-pattern",
            "line-offset",
            "line-translate",
            "line-cap",
            "line-join",
        ],
        "circle" => &[
            "circle-radius",
            "circle-color",
            "circle-opacity",
            "circle-stroke-width",
            "circle-stroke-color",
            "circle-stroke-opacity",
            "circle-blur",
            "circle-translate",
        ],
        "heatmap" => &[
            "heatmap-radius",
            "heatmap-weight",
            "heatmap-intensity",
            "heatmap-color",
            "heatmap-opacity",
        ],
        "hillshade" => &[
            "hillshade-illumination-direction",
            "hillshade-exaggeration",
            "hillshade-shadow-color",
            "hillshade-highlight-color",
        ],
        "raster" => &[
            "raster-opacity",
            "raster-hue-rotate",
            "raster-saturation",
            "raster-contrast",
            "raster-brightness-min",
            "raster-brightness-max",
            "raster-resampling",
        ],
        "symbol" => &[
            "symbol-placement",
            "symbol-spacing",
            "symbol-sort-key",
            "text-field",
            "text-font",
            "text-size",
            "text-color",
            "text-halo-color",
            "text-halo-width",
            "text-opacity",
            "text-anchor",
            "text-offset",
            "text-radial-offset",
            "text-variable-anchor",
            "text-variable-anchor-offset",
            "text-justify",
            "text-transform",
            "text-max-width",
            "text-line-height",
            "text-letter-spacing",
            "text-rotate",
            "text-rotation-alignment",
            "text-max-angle",
            "text-keep-upright",
            "text-padding",
            "text-optional",
            "text-allow-overlap",
            "text-overlap",
            "text-ignore-placement",
            "icon-image",
            "icon-size",
            "icon-rotate",
            "icon-opacity",
            "icon-anchor",
            "icon-offset",
            "icon-padding",
            "icon-text-fit",
            "icon-text-fit-padding",
            "icon-optional",
            "icon-allow-overlap",
            "icon-overlap",
            "icon-ignore-placement",
        ],
        _ => &[],
    }
}

/// Properties that change nothing on a tile, so a converter has nothing to
/// carry over: what follows a camera that a tile never has (a rotated or
/// pitched map, an animation), and `symbol-avoid-edges`, which keeps labels
/// off tile edges where MapLibre would cut them in two, while ezu places
/// labels across tile edges whole.
const NO_EFFECT_ON_A_TILE: &[&str] = &[
    "fill-translate-anchor",
    "fill-extrusion-translate-anchor",
    "line-translate-anchor",
    "circle-translate-anchor",
    "circle-pitch-scale",
    "circle-pitch-alignment",
    "hillshade-illumination-anchor",
    "raster-fade-duration",
    "text-translate-anchor",
    "icon-translate-anchor",
    "text-pitch-alignment",
    "icon-pitch-alignment",
    "symbol-avoid-edges",
];

/// Warn about each paint and layout property of `layer` that its converter
/// neither reads nor needs, so a style is never drawn differently from
/// MapLibre without a word. Run for the types the converter handles; an
/// unsupported type is already skipped with a warning of its own.
pub(crate) fn warn_unread_properties(
    id: &str,
    ty: &str,
    layer: &Map<String, Value>,
    report: &mut crate::maplibre::Report,
) {
    let read = read_properties(ty);
    // A symbol's `icon-*` properties do nothing without an `icon-image`, nor
    // its `text-*` ones without a `text-field`, in MapLibre as here; and how
    // an icon turns with the map or keeps upright along a line matters only
    // on a line, a point-placed icon on an unturned tile being upright.
    let layout = layer.get("layout").and_then(Value::as_object);
    let has = |prop: &str| layout.is_some_and(|l| l.get(prop).is_some_and(|v| !v.is_null()));
    let on_points = layout
        .and_then(|l| l.get("symbol-placement"))
        .and_then(Value::as_str)
        .is_none_or(|p| p == "point");
    let idle = |name: &str| {
        ty == "symbol"
            && ((name.starts_with("icon-") && !has("icon-image"))
                || (name.starts_with("text-") && !has("text-field"))
                || (on_points && matches!(name, "icon-rotation-alignment" | "icon-keep-upright")))
    };
    for section in ["layout", "paint"] {
        let Some(props) = layer.get(section).and_then(Value::as_object) else {
            continue;
        };
        for name in props.keys() {
            let name = name.as_str();
            if name == "visibility"
                || read.contains(&name)
                || NO_EFFECT_ON_A_TILE.contains(&name)
                || idle(name)
            {
                continue;
            }
            report.warn(format!("layer `{id}`: `{name}` is not supported — ignored"));
        }
    }
}

/// MapLibre's `<prefix>-translate` (`circle-translate`, `fill-translate`,
/// `line-translate`): shift the layer's geometry by a constant `[x, y]` in
/// pixels, through a `transform` node between `features` (the `@id` the
/// paint node would read) and the paint node. Returns the reference the
/// paint node reads instead — `features` itself when there is nothing to
/// shift.
///
/// `<prefix>-translate-anchor` needs nothing: it only says whether the
/// shift turns with the map, and a tile is never turned.
pub(crate) fn translated(
    id: &str,
    prop: &str,
    paint: &Map<String, Value>,
    features: String,
    nodes: &mut Map<String, Value>,
    report: &mut crate::maplibre::Report,
) -> String {
    let Some(v) = paint.get(prop) else {
        return features;
    };
    let pair = match v.as_array().map(Vec::as_slice) {
        Some([Value::String(op), Value::Array(inner)]) if op == "literal" => Some(inner.as_slice()),
        Some(arr) => Some(arr),
        None => None,
    }
    .and_then(|arr| match arr {
        [x, y] => Some([x.as_f64()?, y.as_f64()?]),
        _ => None,
    });
    let Some([x, y]) = pair else {
        report.warn(format!(
            "layer `{id}`: `{prop}` carries over only as a constant `[x, y]` — drawn unshifted"
        ));
        return features;
    };
    if x == 0.0 && y == 0.0 {
        return features;
    }
    let shift_id = format!("{id}__translate");
    nodes.insert(
        shift_id.clone(),
        serde_json::json!({ "op": "transform", "features": features, "translate-px": [x, y] }),
    );
    format!("@{shift_id}")
}

pub(crate) fn paint_of(layer: &Map<String, Value>) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    layer
        .get("paint")
        .and_then(Value::as_object)
        .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}
