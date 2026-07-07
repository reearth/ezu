//! `symbol` layer → icon (`layout.icon-image` → sprite `stamp`) and/or
//! text (`layout.text-field` → the `text` node). Icons are placed at
//! the layer's point features; text follows `symbol-placement` (points,
//! or along polylines for `line` / `line-center`). An icon+text layer
//! emits both, text blended over the icon.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::maplibre::filter;
use crate::maplibre::layers::fill::{resolve_number, resolve_paint_color};
use crate::maplibre::layers::paint_of;
use crate::maplibre::sources::{features_node, resolve_layer_source, Sources};
use crate::maplibre::{Report, ZoomRange};

/// MapLibre's default `text-font` stack, used when a layer omits it.
const DEFAULT_TEXT_FONT: [&str; 2] = ["Open Sans Regular", "Arial Unicode MS Regular"];

/// A `symbol` layer: place the `icon-image` sprite (`features` → `icon`
/// → `stamp`) and/or the `text-field` label (`features` → `text`) at
/// each point feature. A `text-font` entry mapped to a font URL via
/// [`ConvertOptions::fonts`](crate::maplibre::ConvertOptions) becomes a
/// `font` source; a stack with no mapping falls back to the style's
/// top-level `glyphs` endpoint (`glyphs_url`) as an SDF `glyphs` source
/// — zero configuration. No mapping and no `glyphs` skips the text
/// with a warning.
#[allow(clippy::too_many_arguments)]
pub(crate) fn convert_symbol(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    zoom_range: ZoomRange,
    sources: &Sources,
    source_defs: &mut Map<String, Value>,
    fonts: &HashMap<String, String>,
    glyphs_url: Option<&str>,
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

    if icon_image.is_none() && !has_text {
        report.warn(format!(
            "layer `{id}`: `symbol` without `icon-image` or `text-field` — skipped"
        ));
        return;
    }

    let base_filter_expr = filter::layer_filter_expr(layer, report, id);
    // Shared by the icon and text nodes; created on first use.
    let feat_id = format!("{id}__feat");
    let mut feat_emitted = false;
    let mut ensure_feat = |nodes: &mut Map<String, Value>| {
        if !feat_emitted {
            nodes.insert(
                feat_id.clone(),
                features_node(
                    &source,
                    &source_layer,
                    base_filter_expr.clone(),
                    min_zoom,
                    max_zoom,
                ),
            );
            feat_emitted = true;
        }
        format!("@{feat_id}")
    };

    if let Some(icon_image) = icon_image {
        convert_icon(
            id,
            icon_image,
            layer,
            layout,
            nodes,
            outputs,
            sources,
            &mut ensure_feat,
            report,
        );
    }
    if has_text {
        convert_text(
            id,
            layer,
            layout,
            nodes,
            outputs,
            source_defs,
            fonts,
            glyphs_url,
            &source,
            &source_layer,
            base_filter_expr.clone(),
            &mut ensure_feat,
            report,
        );
    }
}

/// The icon half: `layout.icon-image` → `icon` (sprite crop) + `stamp`.
#[allow(clippy::too_many_arguments)]
fn convert_icon(
    id: &str,
    icon_image: &Value,
    layer: &Map<String, Value>,
    layout: Option<&Map<String, Value>>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    sources: &Sources,
    ensure_feat: &mut impl FnMut(&mut Map<String, Value>) -> String,
    report: &mut Report,
) {
    let feat_ref = ensure_feat(nodes);
    let stamp_id = format!("{id}__stamp");
    // A constant `icon-image` crops one named icon up front (`icon` node →
    // `stamp` image). A data-driven one is passed to `stamp` as a `name-expr`
    // over the sheet's atlas, cropping each feature's icon at eval time — no
    // per-icon enumeration, since any icon in the bound sheet is croppable.
    let mut spec = match icon_image.as_str() {
        Some(icon_name) => {
            let Some((sprite_src, sprite_icon)) = sources.resolve_icon(icon_name) else {
                report.warn(format!(
                    "layer `{id}`: icon `{icon_name}` needs a `sprite`, but the style declares none — skipped"
                ));
                return;
            };
            let icon_id = format!("{id}__icon");
            nodes.insert(
                icon_id.clone(),
                serde_json::json!({ "op": "icon", "sprite": format!("@{sprite_src}"), "name": sprite_icon }),
            );
            serde_json::json!({ "op": "stamp", "features": feat_ref, "image": format!("@{icon_id}") })
        }
        None => {
            let Some(sprite_src) = sources.default_sprite() else {
                report.warn(format!(
                    "layer `{id}`: data-driven `icon-image` needs a `sprite`, but the style declares none — skipped"
                ));
                return;
            };
            serde_json::json!({
                "op": "stamp", "features": feat_ref,
                "sprite": format!("@{sprite_src}"), "name-expr": icon_image.clone()
            })
        }
    };

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

    // Icon collision is not modelled yet (only text collides).
    for prop in [
        "icon-allow-overlap",
        "icon-ignore-placement",
        "icon-overlap",
    ] {
        if layout.and_then(|l| l.get(prop)).is_some() {
            report.warn(format!(
                "layer `{id}`: `{prop}` not supported — icons are placed without collision"
            ));
        }
    }

    nodes.insert(stamp_id.clone(), spec);
    outputs.push(stamp_id);
}

/// The text half: `layout.text-field` (+ text paint/layout properties)
/// → the `text` node.
#[allow(clippy::too_many_arguments)]
fn convert_text(
    id: &str,
    layer: &Map<String, Value>,
    layout: Option<&Map<String, Value>>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    source_defs: &mut Map<String, Value>,
    fonts: &HashMap<String, String>,
    glyphs_url: Option<&str>,
    source: &str,
    source_layer: &str,
    base_filter_expr: Option<Value>,
    ensure_feat: &mut impl FnMut(&mut Map<String, Value>) -> String,
    report: &mut Report,
) {
    let get = |key: &str| layout.and_then(|l| l.get(key));

    // `symbol-placement`: `point` labels each point feature; `line` /
    // `line-center` walk the layer's polylines with tangent-rotated
    // glyphs. Anything else falls back to point with a warning.
    let placement = get("symbol-placement")
        .and_then(Value::as_str)
        .unwrap_or("point");
    let placement = match placement {
        "point" | "line" | "line-center" => placement,
        other => {
            report.warn(format!(
                "layer `{id}`: unknown `symbol-placement: {other}` — using point placement"
            ));
            "point"
        }
    };
    if get("text-variable-anchor").is_some() {
        report.warn(format!(
            "layer `{id}`: `text-variable-anchor` not supported — using `text-anchor`"
        ));
    }

    // `text-font`: a static string array (or absent → default) lowers to a
    // single stack. A data-driven expression / legacy function (A) is
    // enumerated for the literal stacks it can yield; each is lowered and
    // registered under its canonical key in `font-stacks`, the raw expression
    // is emitted as `font-expr`, and the first stack becomes the required
    // default `font`. Unenumerable expressions fall back to the default stack
    // (renders more of the map than the old "skip whole layer").
    let text_font = get("text-font");
    // A static stack is a literal font-name array; an expression (even one
    // that is syntactically an all-string array, e.g. `["get", "x"]`) is
    // data-driven. `is_expression` is a head check against the operator set,
    // matching how MapLibre itself disambiguates the two. A legacy function
    // object also takes the data-driven path (its stacks come from `stops`).
    let is_static_stack = match text_font {
        None => true,
        Some(v @ Value::Array(_)) => !maplibre_expr::is_expression(v),
        _ => false,
    };
    let mut font_expr_value: Option<Value> = None;
    let mut font_stacks_obj = Map::new();
    let font_refs: Vec<String> = if is_static_stack {
        let stack: Vec<String> = match text_font {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => DEFAULT_TEXT_FONT.iter().map(|s| s.to_string()).collect(),
        };
        match lower_stack(&stack, source_defs, fonts, glyphs_url, id, report) {
            Some(refs) => refs,
            None => return,
        }
    } else {
        let value = text_font.expect("non-static text-font is present");
        let stacks = collect_font_stacks(value);
        if stacks.is_empty() {
            report.warn(format!(
                "layer `{id}`: data-driven `text-font`: no literal stacks found — using the default stack"
            ));
            let stack: Vec<String> = DEFAULT_TEXT_FONT.iter().map(|s| s.to_string()).collect();
            match lower_stack(&stack, source_defs, fonts, glyphs_url, id, report) {
                Some(refs) => refs,
                None => return,
            }
        } else {
            // Lower every enumerated stack; the first that lowers is the
            // default `font`, all that lower populate the registry.
            let mut default_refs: Option<Vec<String>> = None;
            for stack in &stacks {
                let Some(refs) = lower_stack(stack, source_defs, fonts, glyphs_url, id, report)
                else {
                    continue;
                };
                if default_refs.is_none() {
                    default_refs = Some(refs.clone());
                }
                let key = stack.iter().map(|s| s.trim()).collect::<Vec<_>>().join(",");
                font_stacks_obj.entry(key).or_insert_with(|| {
                    Value::Array(refs.iter().cloned().map(Value::from).collect())
                });
            }
            match default_refs {
                Some(refs) => {
                    font_expr_value = Some(value.clone());
                    refs
                }
                // No stack lowered (no `--font` mapping and no `glyphs`
                // endpoint) — `lower_stack` already warned per stack.
                None => return,
            }
        }
    };
    let font_value = Value::Array(font_refs.into_iter().map(Value::from).collect());

    // `text-field`: a constant may carry `{token}`s (rewritten to a
    // `concat`-of-`get` expression); expressions / legacy functions pass
    // through raw. A `format` expression passes through too: the `text` node
    // renders its sections natively (font / scale / colour / vertical-align),
    // so we only register each section's `text-font` in the stack registry.
    let text_value = match get("text-field") {
        Some(Value::String(s)) => match rewrite_field_tokens(s) {
            Some(expr) => expr,
            None => Value::String(s.clone()),
        },
        Some(v @ Value::Array(_)) => {
            register_format_section_fonts(
                v,
                &mut font_stacks_obj,
                source_defs,
                fonts,
                glyphs_url,
                id,
            );
            v.clone()
        }
        // Legacy `{stops}` function: its output strings may carry
        // `{token}`s that the raw passthrough would render literally.
        Some(v @ Value::Object(_)) => match rewrite_legacy_stops_tokens(v) {
            Some(expr) => expr,
            None => v.clone(),
        },
        _ => return,
    };

    let feat_ref = ensure_feat(nodes);
    let mut spec = serde_json::json!({
        "op": "text", "features": feat_ref,
        "font": font_value, "text": text_value
    });
    // The raw data-driven `text-font` expression, if any …
    if let Some(expr) = font_expr_value {
        spec["font-expr"] = expr;
    }
    // … and the stack registry: enumerated `font-expr` stacks and/or `format`
    // section `text-font`s. Emitted whenever non-empty (a `format` label can
    // need it without a `font-expr`).
    if !font_stacks_obj.is_empty() {
        spec["font-stacks"] = Value::Object(font_stacks_obj);
    }

    // Paint / size: constant → plain field, expression → `*-expr`.
    let paint = paint_of(layer);
    let (size, size_expr) = resolve_number(get("text-size"));
    if let Some(s) = size {
        spec["size"] = Value::from(s);
    }
    if let Some(e) = size_expr {
        spec["size-expr"] = e;
    }
    let (color, color_expr) = resolve_paint_color(paint.get("text-color"));
    if let Some(c) = color {
        spec["color"] = Value::from(c);
    }
    if let Some(e) = color_expr {
        spec["color-expr"] = e;
    }
    let (halo_color, halo_color_expr) = resolve_paint_color(paint.get("text-halo-color"));
    if let Some(c) = halo_color {
        spec["halo-color"] = Value::from(c);
    }
    if let Some(e) = halo_color_expr {
        spec["halo-color-expr"] = e;
    }
    let (halo_width, halo_width_expr) = resolve_number(paint.get("text-halo-width"));
    if let Some(w) = halo_width {
        spec["halo-width"] = Value::from(w);
    }
    if let Some(e) = halo_width_expr {
        spec["halo-width-expr"] = e;
    }
    let (opacity, opacity_expr) = resolve_number(paint.get("text-opacity"));
    if let Some(a) = opacity {
        spec["opacity"] = Value::from(a);
    }
    if let Some(e) = opacity_expr {
        spec["opacity-expr"] = e;
    }

    // Layout constants. The `text` node takes these at build time, so an
    // expression here falls back to the default with a warning.
    if let Some(anchor) = const_string(get("text-anchor"), "text-anchor", id, report) {
        spec["anchor"] = Value::from(anchor);
    }
    if let Some(justify) = const_string(get("text-justify"), "text-justify", id, report) {
        spec["justify"] = Value::from(justify);
    }
    if let Some(transform) = const_string(get("text-transform"), "text-transform", id, report) {
        spec["transform"] = Value::from(transform);
    }
    if let Some(offset) = const_offset(get("text-offset"), id, report) {
        spec["offset-em"] = serde_json::json!(offset);
    }
    if let Some(w) = const_number(get("text-max-width"), "text-max-width", id, report) {
        spec["max-width-em"] = Value::from(w);
    }
    if let Some(h) = const_number(get("text-line-height"), "text-line-height", id, report) {
        spec["line-height"] = Value::from(h);
    }
    if let Some(s) = const_number(
        get("text-letter-spacing"),
        "text-letter-spacing",
        id,
        report,
    ) {
        spec["letter-spacing-em"] = Value::from(s);
    }

    // Line placement and its layout knobs (point placement is the node
    // default and ignores them).
    if placement != "point" {
        spec["placement"] = Value::from(placement);
        if let Some(s) = const_number(get("symbol-spacing"), "symbol-spacing", id, report) {
            spec["spacing-px"] = Value::from(s);
        }
        if let Some(a) = const_number(get("text-max-angle"), "text-max-angle", id, report) {
            spec["max-angle-deg"] = Value::from(a);
        }
        if get("text-keep-upright").and_then(Value::as_bool) == Some(false) {
            spec["keep-upright"] = Value::from(false);
        }
        // Glyphs always rotate with the line in ezu (map alignment); a
        // viewport-aligned line label has no static-renderer equivalent.
        if get("text-rotation-alignment").and_then(Value::as_str) == Some("viewport") {
            report.warn(format!(
                "layer `{id}`: `text-rotation-alignment: viewport` on line placement not supported — glyphs follow the line"
            ));
        }
    }

    // Collision (deterministic cross-tile placement). Always thread the
    // origin source/layer (+ the layer filter) through so the `text` node
    // gathers neighbour candidates and filters them exactly like its own
    // features; collision itself is on by default in the `text` node.
    spec["source"] = Value::from(source);
    spec["layer"] = Value::from(source_layer);
    if let Some(f) = base_filter_expr {
        spec["filter-expr"] = f;
    }

    // `text-allow-overlap` (bool), superseded by the newer `text-overlap`
    // enum when present: `always` → allow, `never`/absent → collide,
    // `cooperative` → treated as `never` with a warning (no cooperative
    // fade model here).
    let mut allow_overlap = get("text-allow-overlap").and_then(Value::as_bool);
    match get("text-overlap").and_then(Value::as_str) {
        Some("always") => allow_overlap = Some(true),
        Some("never") => allow_overlap = Some(false),
        Some("cooperative") => {
            report.warn(format!(
                "layer `{id}`: `text-overlap: cooperative` has no ezu equivalent — treated as `never`"
            ));
            allow_overlap = Some(false);
        }
        Some(other) => report.warn(format!(
            "layer `{id}`: unknown `text-overlap: {other}` — using collision default"
        )),
        None => {}
    }
    if allow_overlap == Some(true) {
        spec["allow-overlap"] = Value::from(true);
    }

    if get("text-ignore-placement").and_then(Value::as_bool) == Some(true) {
        spec["ignore-placement"] = Value::from(true);
    }
    if let Some(p) = const_number(get("text-padding"), "text-padding", id, report) {
        spec["padding-px"] = Value::from(p);
    }
    // `symbol-sort-key`: constant or expression — the `text` node parses
    // either on `sort-key-expr`.
    if let Some(v) = get("symbol-sort-key") {
        spec["sort-key-expr"] = v.clone();
    }
    // Text/icon pairing has no ezu counterpart yet.
    if get("text-optional").is_some() {
        report.warn(format!(
            "layer `{id}`: `text-optional` (icon/text pairing) not supported — text placed independently"
        ));
    }

    let text_id = format!("{id}__text");
    nodes.insert(text_id.clone(), spec);
    outputs.push(text_id);
}

/// Reuse (by URL) or declare a `font` source for one fontstack entry.
/// Returns the source id, derived from the entry name.
/// Lower one MapLibre font stack to ezu `font`/`glyphs` source names.
/// Mapped entries (present in the `--font` table) become `font` sources; if
/// none are mapped, the whole stack becomes one `glyphs` source over
/// `glyphs_url` (its `fontstack` = names joined `", "`, MapLibre's server
/// convention). Neither available → `None` (warned). Shared by the static
/// `text-font` path and each enumerated dynamic stack.
fn lower_stack(
    stack: &[String],
    source_defs: &mut Map<String, Value>,
    fonts: &HashMap<String, String>,
    glyphs_url: Option<&str>,
    id: &str,
    report: &mut Report,
) -> Option<Vec<String>> {
    let (mapped, unmapped): (Vec<&String>, Vec<&String>) =
        stack.iter().partition(|name| fonts.contains_key(*name));
    if mapped.is_empty() {
        // Zero-config compat: serve the stack from the style's glyph
        // endpoint as SDF ranges (server-side fallback, as in MapLibre).
        let Some(glyphs_url) = glyphs_url else {
            report.warn(format!(
                "layer `{id}`: `symbol` text: no font mapping for {stack:?} and the style has no `glyphs` endpoint — pass `--font \"NAME=URL\"`; text skipped"
            ));
            return None;
        };
        Some(vec![ensure_glyphs_source(
            source_defs,
            glyphs_url,
            &stack.join(", "),
        )])
    } else {
        // An explicit mapping wins over the `glyphs` endpoint.
        if !unmapped.is_empty() {
            report.warn(format!(
                "layer `{id}`: `symbol` text: no font mapping for {unmapped:?} — using the mapped subset"
            ));
        }
        Some(
            mapped
                .iter()
                .map(|name| {
                    let url = fonts
                        .get(name.as_str())
                        .expect("partitioned on containment");
                    ensure_font_source(source_defs, name, url)
                })
                .collect(),
        )
    }
}

/// A JSON array all of whose elements are strings → the owned name list.
fn as_string_array(v: &Value) -> Option<Vec<String>> {
    let a = v.as_array()?;
    let mut names = Vec::with_capacity(a.len());
    for x in a {
        names.push(x.as_str()?.to_string());
    }
    Some(names)
}

/// Enumerate the literal font stacks a data-driven `text-font` value can
/// yield, in document order, deduped: every `["literal", [<strings>]]` in the
/// expression tree, plus a legacy function's `stops` outputs and `default`
/// (both string arrays). MapLibre likewise requires data-driven `text-font`
/// outputs to be literals, so a syntactic scan is faithful; anything it misses
/// falls back to the default stack at eval.
fn collect_font_stacks(v: &Value) -> Vec<Vec<String>> {
    fn push_unique(out: &mut Vec<Vec<String>>, names: Vec<String>) {
        if !names.is_empty() && !out.contains(&names) {
            out.push(names);
        }
    }
    fn rec(v: &Value, out: &mut Vec<Vec<String>>) {
        match v {
            Value::Array(a) => {
                // `["literal", <data>]` — the operand is data, not a
                // sub-expression: collect a string array, never recurse in.
                if a.len() == 2 && a[0].as_str() == Some("literal") {
                    if let Some(names) = as_string_array(&a[1]) {
                        push_unique(out, names);
                    }
                    return;
                }
                for x in a {
                    rec(x, out);
                }
            }
            Value::Object(m) => {
                // Legacy `{stops}` function: each stop is `[input, output]`.
                if let Some(Value::Array(stops)) = m.get("stops") {
                    for stop in stops {
                        if let Some(output) = stop.as_array().and_then(|p| p.get(1)) {
                            if let Some(names) = as_string_array(output) {
                                push_unique(out, names);
                            }
                        }
                    }
                }
                if let Some(names) = m.get("default").and_then(as_string_array) {
                    push_unique(out, names);
                }
                for val in m.values() {
                    rec(val, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    rec(v, &mut out);
    out
}

/// Walk a `text-field` value and register every `format` section's `text-font`
/// in `font_stacks` (keyed by its canonical `,`-joined name, resolved through
/// [`lower_stack`]). Descends the whole tree, so `format`s nested in `case` /
/// `match` (as the Protomaps multi-script labels use) are found too.
///
/// Section-font lowering is best-effort and quiet: a stack that can't be
/// mapped is simply not registered (the `text` node falls back to the layer's
/// default stack for that section), so `lower_stack`'s warnings are discarded
/// rather than spamming one "text skipped" per unmapped section — the label's
/// text is not skipped.
fn register_format_section_fonts(
    v: &Value,
    font_stacks: &mut Map<String, Value>,
    source_defs: &mut Map<String, Value>,
    fonts: &HashMap<String, String>,
    glyphs_url: Option<&str>,
    id: &str,
) {
    match v {
        Value::Array(arr) => {
            if arr.first().and_then(Value::as_str) == Some("format") {
                // `["format", content0, style0, content1, style1, …]` — style
                // objects sit at the even indices from 2.
                let mut i = 2;
                while i < arr.len() {
                    if let Some(obj) = arr[i].as_object() {
                        if let Some(tf) = obj.get("text-font") {
                            // A section `text-font` is a literal stack (bare
                            // array or `["literal", […]]`) or a small
                            // expression; enumerate its stacks like the layer's.
                            let stacks = match as_string_array(tf) {
                                Some(s) => vec![s],
                                None => collect_font_stacks(tf),
                            };
                            let mut quiet = Report::default();
                            for stack in &stacks {
                                if let Some(refs) = lower_stack(
                                    stack,
                                    source_defs,
                                    fonts,
                                    glyphs_url,
                                    id,
                                    &mut quiet,
                                ) {
                                    let key = stack
                                        .iter()
                                        .map(|s| s.trim())
                                        .collect::<Vec<_>>()
                                        .join(",");
                                    font_stacks.entry(key).or_insert_with(|| {
                                        Value::Array(
                                            refs.iter().cloned().map(Value::from).collect(),
                                        )
                                    });
                                }
                            }
                        }
                    }
                    i += 2;
                }
            }
            for x in arr {
                register_format_section_fonts(x, font_stacks, source_defs, fonts, glyphs_url, id);
            }
        }
        Value::Object(m) => {
            for val in m.values() {
                register_format_section_fonts(val, font_stacks, source_defs, fonts, glyphs_url, id);
            }
        }
        _ => {}
    }
}

fn ensure_font_source(source_defs: &mut Map<String, Value>, name: &str, url: &str) -> String {
    // One source per distinct URL, shared across layers and stacks.
    if let Some((id, _)) = source_defs
        .iter()
        .find(|(_, d)| d["type"] == "font" && d["url"] == url)
    {
        return id.clone();
    }
    let id = unique_source_id(source_defs, &kebab_id(name));
    source_defs.insert(
        id.clone(),
        serde_json::json!({ "type": "font", "url": url }),
    );
    id
}

/// Reuse or declare a `glyphs` source for one fontstack string served
/// from the style's glyph endpoint. Returns the source id, derived
/// from the joined stack.
fn ensure_glyphs_source(
    source_defs: &mut Map<String, Value>,
    url: &str,
    fontstack: &str,
) -> String {
    // One source per distinct (endpoint, fontstack), shared across layers.
    if let Some((id, _)) = source_defs
        .iter()
        .find(|(_, d)| d["type"] == "glyphs" && d["url"] == url && d["fontstack"] == fontstack)
    {
        return id.clone();
    }
    let id = unique_source_id(source_defs, &kebab_id(fontstack));
    source_defs.insert(
        id.clone(),
        serde_json::json!({ "type": "glyphs", "url": url, "fontstack": fontstack }),
    );
    id
}

/// `"Noto Sans Regular"` → `"noto-sans-regular"` (source-id shape).
fn kebab_id(name: &str) -> String {
    let mut base: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while base.contains("--") {
        base = base.replace("--", "-");
    }
    base.trim_matches('-').to_string()
}

/// `base`, suffixed on collision with an unrelated source name.
fn unique_source_id(source_defs: &Map<String, Value>, base: &str) -> String {
    let mut id = base.to_string();
    let mut n = 2;
    while source_defs.contains_key(&id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

/// Rewrite a constant `text-field` carrying `{token}`s into a MapLibre
/// expression: `{name}` → `["to-string", ["get", "name"]]`, mixed text →
/// `["concat", …]`. Returns `None` when the string has no tokens.
fn rewrite_field_tokens(s: &str) -> Option<Value> {
    let mut parts: Vec<Value> = Vec::new();
    let mut literal = String::new();
    let mut rest = s;
    let mut found = false;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            break; // unclosed brace: literal from here on
        };
        let token = &after[..close];
        literal.push_str(&rest[..open]);
        if !literal.is_empty() {
            parts.push(Value::String(std::mem::take(&mut literal)));
        }
        parts.push(serde_json::json!(["to-string", ["get", token]]));
        found = true;
        rest = &after[close + 1..];
    }
    if !found {
        return None;
    }
    literal.push_str(rest);
    if !literal.is_empty() {
        parts.push(Value::String(literal));
    }
    if parts.len() == 1 {
        return Some(parts.pop().expect("one part"));
    }
    let mut concat = vec![Value::String("concat".into())];
    concat.extend(parts);
    Some(Value::Array(concat))
}

/// Rewrite a legacy zoom-interval `{stops}` `text-field` whose output
/// strings carry `{token}`s into a `["step", ["zoom"], …]` expression
/// with each output token-expanded (legacy interval semantics — the
/// first output also covers zooms below the first stop — match `step`).
/// Returns `None` when nothing needs rewriting or the function isn't a
/// plain zoom-interval string function (data-driven `property`,
/// `categorical`, non-string outputs): those pass through raw as before.
fn rewrite_legacy_stops_tokens(v: &Value) -> Option<Value> {
    let obj = v.as_object()?;
    if obj.contains_key("property") {
        return None;
    }
    match obj.get("type").and_then(Value::as_str) {
        None | Some("interval") => {}
        Some(_) => return None,
    }
    let stops = obj.get("stops")?.as_array()?;
    let mut pairs: Vec<(f64, &str)> = Vec::with_capacity(stops.len());
    for stop in stops {
        let pair = stop.as_array()?;
        pairs.push((pair.first()?.as_f64()?, pair.get(1)?.as_str()?));
    }
    if pairs.is_empty() || !pairs.iter().any(|(_, s)| s.contains('{')) {
        return None;
    }
    let expand = |s: &str| rewrite_field_tokens(s).unwrap_or_else(|| Value::String(s.into()));
    if pairs.len() == 1 {
        return Some(expand(pairs[0].1));
    }
    let mut step = vec![
        Value::String("step".into()),
        serde_json::json!(["zoom"]),
        expand(pairs[0].1),
    ];
    for (input, output) in &pairs[1..] {
        step.push(Value::from(*input));
        step.push(expand(output));
    }
    Some(Value::Array(step))
}

/// A constant string layout property; an expression warns and yields
/// `None` (the node default applies).
fn const_string(v: Option<&Value>, prop: &str, id: &str, report: &mut Report) -> Option<String> {
    match v {
        None => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            report.warn(format!(
                "layer `{id}`: expression `{prop}` not supported — using the default"
            ));
            None
        }
    }
}

/// A constant numeric layout property; an expression warns and yields
/// `None` (the node default applies).
fn const_number(v: Option<&Value>, prop: &str, id: &str, report: &mut Report) -> Option<f64> {
    match v {
        None => None,
        Some(n) if n.is_number() => n.as_f64(),
        Some(_) => {
            report.warn(format!(
                "layer `{id}`: expression `{prop}` not supported — using the default"
            ));
            None
        }
    }
}

/// A constant `text-offset` (`[x, y]` in em); an expression warns and
/// yields `None`.
fn const_offset(v: Option<&Value>, id: &str, report: &mut Report) -> Option<[f64; 2]> {
    match v {
        None => None,
        Some(Value::Array(a)) if a.len() == 2 && a.iter().all(Value::is_number) => {
            Some([a[0].as_f64()?, a[1].as_f64()?])
        }
        Some(_) => {
            report.warn(format!(
                "layer `{id}`: expression `text-offset` not supported — using the default"
            ));
            None
        }
    }
}
