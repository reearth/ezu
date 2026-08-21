//! Convert [MapLibre GL styles] into **ezu recipes** — the node-DAG
//! [`Document`](ezu-style) JSON that ezu renders on the CPU.
//!
//! MapLibre is an ordered list of layers whose paint/layout properties
//! are computed per feature and per (fractional) zoom via *expressions*.
//! ezu is a typed node DAG whose ops are styled uniformly. The two models
//! differ deeply, so this converter targets the tractable subset first
//! (see the crate README / `project_maplibre_conversion` notes):
//!
//! - **Layer list → `stack`.** The painter's algorithm (each layer
//!   drawn over the last) folds into a single n-ary `stack` node.
//! - **background / fill / line / raster** layer types.
//! - **`match` on `["get", prop]`** for fill colour → one filtered
//!   `fill-solid` per colour bucket (ezu membership filters do this
//!   cleanly).
//! - **filters**: `all` + `==` / `!=` / `in` / `!in`.
//! - **zoom / data functions** (legacy `stops`, `interpolate`, `step`, and
//!   any other MapLibre expression) are emitted **raw** onto the target
//!   node's `*-expr` field and evaluated per tile by ezu-paint (via
//!   `maplibre-expr`, with the tile's zoom in the evaluation context). The
//!   converter never bakes them to a constant, so one recipe renders
//!   correctly at every zoom. Layer `minzoom`/`maxzoom` become the
//!   `features` node's `min-zoom`/`max-zoom` render-time gate — converted
//!   rather than copied, since MapLibre's upper bound is exclusive and
//!   ezu's is not.
//!
//! - **sprites**: a top-level `sprite` (single URL or `[{id, url}]`
//!   sheets) becomes `sprite` source(s); `symbol` **icons**,
//!   `fill-pattern`, and `line-pattern` wire through `icon` (crop) +
//!   `stamp` / `tiling` / `line-stamp`.
//! - **`symbol` text**: `text-field` and its paint / layout properties
//!   lower to the `text` node — `symbol-placement: point` at each
//!   feature point, `line` / `line-center` along each polyline (with
//!   `symbol-spacing` / `text-max-angle` / `text-keep-upright`). A
//!   `text-font` stack with no [`ConvertOptions::fonts`] mapping is
//!   served from the style's own top-level `glyphs` endpoint as an SDF
//!   `glyphs` source — zero configuration; an explicit font-URL mapping
//!   wins and gives higher-fidelity outline rendering.
//!
//! What is *not* handled yet is reported in [`Report::warnings`] rather
//! than failing the conversion: text variable anchors, icon collision,
//! and expression operators outside the set above. Inline/remote
//! `geojson` sources *are* converted (the host projects them into each
//! tile).
//!
//! [MapLibre GL styles]: https://maplibre.org/maplibre-style-spec/
//! [`Document`]: https://docs.rs/ezu-style

use serde_json::{Map, Value};

pub(crate) mod color;
pub(crate) mod filter;
pub(crate) mod layers;
pub(crate) mod sources;

use layers::{
    convert_background, convert_circle, convert_fill, convert_fill_extrusion, convert_heatmap,
    convert_hillshade, convert_line, convert_raster, convert_symbol, emit_label_placement,
};
use sources::convert_sources;

// --- MapLibre paint-value classification ------------------------------------
//
// ezu recipes are zoom-independent: a zoom/data *function* is emitted as a raw
// MapLibre expression on the target node's `*-expr` field and evaluated per
// tile by ezu-paint (via `maplibre-expr`, with the tile's zoom in the eval
// context). So the converter never bakes a function to a constant — it only
// tells a *plain literal constant* apart from *any function/expression*.

/// A plain numeric constant: a bare JSON number. Arrays/objects are
/// functions/expressions, not constants.
pub(crate) fn const_number(v: &Value) -> Option<f64> {
    v.as_f64()
}

/// A plain colour constant: a literal colour string.
pub(crate) fn const_color(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// Whether the value is a MapLibre expression (array) or a legacy function
/// object (`{stops}`, …) — i.e. it routes to a `*-expr` field, not a constant.
pub(crate) fn is_expr(v: &Value) -> bool {
    v.is_array() || v.is_object()
}

/// Knobs controlling how a MapLibre style is lowered to an ezu recipe.
#[derive(Debug, Clone)]
pub struct ConvertOptions {
    /// Emitted `tile-size`. MapLibre uses 512px tiles.
    pub tile_size: u32,
    /// Emitted `pad` — the margin where geometry painted wider than its
    /// own extent lands before the crop.
    ///
    /// A renderer sizes the canvas for how far each filter *reads* on its
    /// own, so this is not about blur or warp. It is about how far an op
    /// *paints*: MapLibre line widths are usually expressions, which have
    /// no value until a tile renders, so a stroke running just outside the
    /// tile needs a margin declared up front or the pixels it should cover
    /// inside the tile come out unpainted. The default covers the widths a
    /// typical basemap draws.
    pub pad: u32,
    /// How to treat `layout.visibility: "none"` layers. `false` (default)
    /// drops them. `true` keeps their nodes in the recipe but gates each
    /// behind a `switch` that defaults to a transparent branch — so the
    /// layer is off yet present, and flipping the switch's `select` to `b`
    /// turns it on (a build-time toggle, since `switch` resolves at build).
    /// A hidden label layer also stays out of the shared label placement, so
    /// it knocks nothing out while off; it places its own labels once on.
    pub keep_hidden: bool,
    /// MapLibre fontstack entry name → font source, used to lower
    /// `symbol` text (`text-font`) to ezu `font` sources (CLI:
    /// repeatable `--font NAME=SOURCE`). The source is passed straight
    /// through as the `font` source's `url`, so it may be an
    /// installed-font reference (`system:Helvetica`) or a font-file URL
    /// (`http(s)://…`, `file:…`, `data:…`). Optional: a stack with no
    /// mapped entry falls back to the style's top-level `glyphs`
    /// endpoint as an SDF `glyphs` source (MapLibre's own rendering
    /// path); a mapping wins where present and renders from the real
    /// font. No mapping *and* no `glyphs` skips the text with a warning.
    pub fonts: std::collections::HashMap<String, String>,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            tile_size: 512,
            pad: 16,
            keep_hidden: false,
            fonts: std::collections::HashMap::new(),
        }
    }
}

/// A layer's zoom gate as an ezu `features` node wants it — inclusive at
/// both ends — threaded on as `min-zoom`/`max-zoom`. `None` where the
/// layer omits the bound. Built by [`layer_zoom_range`], which converts
/// MapLibre's half-open range; the two are not the same numbers.
pub(crate) type ZoomRange = (Option<u8>, Option<u8>);

/// Convert a layer's MapLibre zoom bounds into the inclusive band an ezu
/// `features` node gates on, clamped to ezu's `0..=24` zoom range.
///
/// MapLibre shows a layer for `minzoom <= z < maxzoom` — inclusive below,
/// **exclusive above** — while `features` draws for
/// `min-zoom <= z <= max-zoom`, inclusive at both ends. Rendered zooms are
/// whole numbers, so the exclusive bound is one level lower: `maxzoom: 12`
/// draws through z11, not z12.
///
/// Both bounds take the *ceiling* rather than rounding, because a
/// fractional bound is a threshold and not an approximate level.
/// `minzoom: 12.4` first shows at z13 (z12 is below the threshold), and
/// `maxzoom: 12.5` last shows at z12. Rounding would put both a level out
/// whenever the fraction fell below .5.
///
/// Returns `None` when the declared band holds no whole zoom at all —
/// `maxzoom: 0`, or a `maxzoom` at or below `minzoom`. MapLibre never
/// shows such a layer, so there is nothing to convert.
fn layer_zoom_range(layer: &Map<String, Value>) -> Option<ZoomRange> {
    let read = |key| {
        layer
            .get(key)
            .and_then(Value::as_f64)
            .filter(|z| z.is_finite())
    };
    let min = read("minzoom").map(|z| z.ceil().clamp(0.0, 24.0) as u8);
    let max = match read("maxzoom") {
        // Clamp before stepping down so `maxzoom: 0` lands below zero and
        // is rejected, while an over-large bound saturates instead.
        Some(z) => match z.ceil().clamp(0.0, 25.0) - 1.0 {
            top if top < 0.0 => return None,
            top => Some(top.min(24.0) as u8),
        },
        None => None,
    };
    if let (Some(mn), Some(mx)) = (min, max) {
        if mn > mx {
            return None;
        }
    }
    Some((min, max))
}

/// Non-fatal notes accumulated during conversion: layers or properties
/// that were skipped or approximated. Surface these to the user so an
/// imperfect render is explained rather than mysterious.
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub warnings: Vec<String>,
}

impl Report {
    fn warn(&mut self, msg: impl Into<String>) {
        self.warnings.push(msg.into());
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error("style is not a JSON object")]
    NotAnObject,
    #[error("style has no `layers` array")]
    NoLayers,
    #[error("no vector (MVT/PMTiles) source found; ezu needs tiled vector data")]
    NoVectorSource,
}

/// Convert a parsed MapLibre GL style into an ezu recipe (Document JSON).
///
/// Returns the recipe plus a [`Report`] of everything skipped/approximated.
/// The recipe is a plain [`serde_json::Value`]; feed it to
/// `ezu_style::Document::from_json` (or write it to a `.json` and run the
/// `ezu` CLI) to render.
pub fn convert(style: &Value, opts: &ConvertOptions) -> Result<(Value, Report), ConvertError> {
    let style = style.as_object().ok_or(ConvertError::NotAnObject)?;
    let mut report = Report::default();

    // --- sources ---------------------------------------------------------
    let (mut source_defs, sources) = convert_sources(style, &mut report)?;

    // The top-level glyph endpoint — `symbol` text's zero-config
    // fallback for fontstacks without an explicit font mapping.
    let glyphs_url = style.get("glyphs").and_then(Value::as_str);

    // --- layers → paint node chain --------------------------------------
    let layers = style
        .get("layers")
        .and_then(Value::as_array)
        .ok_or(ConvertError::NoLayers)?;

    let mut nodes = Map::new();
    // Ordered list of the top raster node id each layer contributes; folded
    // into a blend chain at the end (painter's algorithm).
    let mut outputs: Vec<String> = Vec::new();
    // Every label layer's `text-labels` node, in style order: they all feed
    // one shared `label-placement` node emitted after the walk, so labels of
    // different layers collide with each other as they do in MapLibre.
    let mut label_layers: Vec<String> = Vec::new();
    // Largest canvas pad (px) any layer's pad-hungry kernel needs. The
    // document `pad` is lifted to cover this so kernels aren't clipped at
    // tile borders; today only `heatmap` → `density` reports a requirement.
    let mut required_pad: u32 = 0;

    for layer in layers {
        let Some(layer) = layer.as_object() else {
            continue;
        };
        let id = layer.get("id").and_then(Value::as_str).unwrap_or("layer");
        // `layout.visibility: "none"`: drop by default, or (with
        // `keep_hidden`) keep the nodes but gate them off via `switch`.
        let hidden = layer
            .get("layout")
            .and_then(|l| l.get("visibility"))
            .and_then(Value::as_str)
            == Some("none");
        if hidden && !opts.keep_hidden {
            continue;
        }
        // MapLibre shows a layer for `minzoom <= z < maxzoom`. ezu recipes
        // are zoom-independent, so rather than dropping the layer at a baked
        // zoom we thread the range onto the `features` node as a render-time
        // gate (`min-zoom`/`max-zoom`), computed once per layer. Note the
        // bounds are not the same numbers — see `layer_zoom_range`.
        let Some(zoom_range) = layer_zoom_range(layer) else {
            report.warn(format!(
                "layer `{id}`: its minzoom/maxzoom band contains no whole zoom level, so \
                 MapLibre would never draw it — skipped"
            ));
            continue;
        };
        let ty = layer.get("type").and_then(Value::as_str).unwrap_or("");
        let out_start = outputs.len();
        match ty {
            "background" => convert_background(id, layer, &mut nodes, &mut outputs),
            "fill" => convert_fill(
                id,
                layer,
                &mut nodes,
                &mut outputs,
                zoom_range,
                &sources,
                &mut report,
            ),
            "line" => convert_line(
                id,
                layer,
                &mut nodes,
                &mut outputs,
                zoom_range,
                &sources,
                &mut report,
            ),
            "raster" => convert_raster(id, layer, &mut nodes, &mut outputs, &mut report),
            "circle" => convert_circle(
                id,
                layer,
                &mut nodes,
                &mut outputs,
                zoom_range,
                &sources,
                &mut report,
            ),
            "heatmap" => {
                required_pad = required_pad.max(convert_heatmap(
                    id,
                    layer,
                    &mut nodes,
                    &mut outputs,
                    zoom_range,
                    &sources,
                    &mut report,
                ));
            }
            "hillshade" => convert_hillshade(id, layer, &mut nodes, &mut outputs, &mut report),
            "fill-extrusion" => convert_fill_extrusion(
                id,
                layer,
                &mut nodes,
                &mut outputs,
                zoom_range,
                &sources,
                &mut report,
            ),
            "symbol" => convert_symbol(
                id,
                layer,
                &mut nodes,
                &mut outputs,
                // A hidden layer stays out of the shared placement index:
                // MapLibre collides only visible symbol layers.
                (!hidden).then_some(&mut label_layers),
                zoom_range,
                &sources,
                &mut source_defs,
                &opts.fonts,
                glyphs_url,
                &mut report,
            ),
            other => report.warn(format!(
                "layer `{id}`: type `{other}` not supported — skipped"
            )),
        }
        // Gate a kept-hidden layer's contributions off via `switch`.
        if hidden {
            gate_hidden(id, &mut nodes, &mut outputs, out_start);
        }
    }

    emit_label_placement(&mut nodes, &label_layers);

    if outputs.is_empty() {
        report.warn("no renderable layers produced output".to_string());
    }
    let output = fold_blend(&mut nodes, &outputs);

    let mut doc = Map::new();
    doc.insert(
        "name".into(),
        Value::String(
            style
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("converted")
                .to_string(),
        ),
    );
    doc.insert("tile-size".into(), Value::from(opts.tile_size));
    // The requested `pad` is a floor: honour a user's larger value, but lift
    // it when a kernel (heatmap radius) needs more buffer than requested,
    // otherwise the kernel is clipped and tiles seam at their borders.
    let pad = if required_pad > opts.pad {
        report.warn(format!(
            "raised the recipe pad from {} to {required_pad}px to cover the heatmap \
             kernel radius (pass a larger `pad` to override)",
            opts.pad
        ));
        required_pad
    } else {
        opts.pad
    };
    doc.insert("pad".into(), Value::from(pad));
    doc.insert("sources".into(), Value::Object(source_defs));
    doc.insert("nodes".into(), Value::Object(nodes));
    doc.insert("output".into(), Value::String(output));

    Ok((Value::Object(doc), report))
}

/// Fold the ordered output node ids into a single `stack` (painter's
/// algorithm) and return the id of the final node.
/// Gate the outputs a hidden layer pushed (`outputs[from..]`) behind a
/// `switch` whose default `select: "a"` picks a shared transparent branch,
/// so the layer is off but present. Set the switch's `select` to `"b"` to
/// turn the layer on.
fn gate_hidden(id: &str, nodes: &mut Map<String, Value>, outputs: &mut [String], from: usize) {
    if from >= outputs.len() {
        return;
    }
    // One shared transparent (fully-clear) solid as the "off" branch.
    const OFF: &str = "__hidden_off";
    if !nodes.contains_key(OFF) {
        nodes.insert(
            OFF.into(),
            serde_json::json!({ "op": "solid", "color": "#00000000" }),
        );
    }
    for (i, slot) in outputs.iter_mut().enumerate().skip(from) {
        let sw = format!("{id}__vis{i}");
        nodes.insert(
            sw.clone(),
            serde_json::json!({
                "op": "switch",
                "a": format!("@{OFF}"),
                "b": format!("@{slot}"),
                "select": "a"
            }),
        );
        *slot = sw;
    }
}

fn fold_blend(nodes: &mut Map<String, Value>, outputs: &[String]) -> String {
    match outputs.split_first() {
        None => {
            // Degenerate: emit a transparent solid so the doc still renders.
            nodes.insert(
                "empty".into(),
                serde_json::json!({ "op": "solid", "color": "#00000000" }),
            );
            "empty".to_string()
        }
        Some((first, rest)) => {
            // Painter's algorithm: every layer is composited with plain
            // source-over onto the ones below, so the whole run collapses
            // into a single n-ary `stack` (one accumulator, one pass)
            // instead of a chain of `blend` nodes.
            let mut layers = vec![first.clone()];
            for over in rest {
                // Compositing a fully opaque solid over itself is a no-op
                // (source-over with alpha 1 replaces the base with identical
                // pixels). This happens when a style seeds the layer list with
                // a redundant background layer, so drop the duplicate instead
                // of stacking it again. Only a *leading* run of duplicates of
                // the bottom layer is collapsed (nothing has been composited
                // over it yet); a translucent duplicate is never dropped —
                // source-over stacks alpha (C = Cs + Cb·(1−αs)), so drawing it
                // twice darkens the result, matching MapLibre's behaviour of
                // rendering duplicate layers twice.
                if layers.len() == 1 && *over == *first && is_opaque_solid(nodes, first) {
                    continue;
                }
                layers.push(over.clone());
            }
            if layers.len() == 1 {
                return layers.into_iter().next().expect("one layer");
            }
            let id = unique_id(nodes, "stack");
            let refs: Vec<Value> = layers
                .iter()
                .map(|l| Value::String(format!("@{l}")))
                .collect();
            emit_stack_chain(nodes, &id, &refs);
            id
        }
    }
}

/// Widest layer list a single `stack` node is given.
///
/// A renderer must hold every input of a node in memory at once, so one
/// `stack` spanning a whole basemap's worth of layers pins a full padded
/// raster per layer — tens of megabytes on a style with dozens of layers,
/// and the peak is the same however the evaluator schedules the work.
/// Splitting the run into a chain of narrow stacks caps that at roughly
/// one chunk plus the accumulator, because each chunk's layers can be
/// composited and released before the next chunk is drawn.
///
/// The pixels are unaffected: source-over is applied to the same layers
/// in the same order either way, and a chunk boundary only materializes
/// the accumulator the next chunk continues from.
const STACK_CHUNK: usize = 8;

/// Emit `layers` as a bottom-to-top chain of `stack` nodes of at most
/// [`STACK_CHUNK`] inputs each, the topmost taking `id` so references to
/// the stack as a whole keep resolving. Runs that already fit stay a
/// single node.
fn emit_stack_chain(nodes: &mut Map<String, Value>, id: &str, layers: &[Value]) {
    let mut pos = 0;
    let mut prev: Option<String> = None;
    let mut part = 0;
    while pos < layers.len() {
        // Every chunk after the first spends one input on the
        // accumulator it continues from.
        let take = STACK_CHUNK - usize::from(prev.is_some());
        let end = (pos + take).min(layers.len());
        let mut chunk: Vec<Value> = Vec::with_capacity(STACK_CHUNK);
        if let Some(p) = &prev {
            chunk.push(Value::String(format!("@{p}")));
        }
        chunk.extend_from_slice(&layers[pos..end]);
        let node_id = if end == layers.len() {
            id.to_string()
        } else {
            part += 1;
            unique_id(nodes, &format!("{id}_{part}"))
        };
        nodes.insert(
            node_id.clone(),
            serde_json::json!({ "op": "stack", "layers": Value::Array(chunk) }),
        );
        prev = Some(node_id);
        pos = end;
    }
}

/// Pick an unused node id: `base`, else `base_1`, `base_2`, …
fn unique_id(nodes: &Map<String, Value>, base: &str) -> String {
    if !nodes.contains_key(base) {
        return base.to_string();
    }
    (1..)
        .map(|n| format!("{base}_{n}"))
        .find(|id| !nodes.contains_key(id))
        .expect("an unused id exists")
}

/// True when `id` names a `solid` node that is certainly fully opaque: a hex
/// colour with no alpha channel (`#rgb` / `#rrggbb`) or an explicit `ff`
/// alpha, and no `opacity` dial below 1. Only such a node is safe to drop
/// from a self-blend — blending a translucent solid over itself changes the
/// result.
fn is_opaque_solid(nodes: &Map<String, Value>, id: &str) -> bool {
    let Some(node) = nodes.get(id).and_then(Value::as_object) else {
        return false;
    };
    if node.get("op").and_then(Value::as_str) != Some("solid") {
        return false;
    }
    if let Some(opacity) = node.get("opacity") {
        if opacity.as_f64() != Some(1.0) {
            return false;
        }
    }
    let Some(hex) = node
        .get("color")
        .and_then(Value::as_str)
        .and_then(|c| c.strip_prefix('#'))
    else {
        return false;
    };
    match hex.len() {
        3 | 6 => true,
        4 => hex.ends_with(['f', 'F']),
        8 => hex[6..].eq_ignore_ascii_case("ff"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(color: &str) -> Value {
        serde_json::json!({ "op": "solid", "color": color })
    }

    #[test]
    fn self_blend_of_an_opaque_solid_is_skipped() {
        let mut nodes = Map::new();
        nodes.insert("bg".into(), solid("#cccccc"));
        nodes.insert("fill".into(), solid("#112233"));
        let outputs = ["bg".to_string(), "bg".to_string(), "fill".to_string()];
        let out = fold_blend(&mut nodes, &outputs);
        assert_eq!(out, "stack");
        // The duplicate opaque `bg` is collapsed away, leaving one stack of
        // the bottom layer and the fill above it.
        assert_eq!(
            nodes["stack"]["layers"],
            serde_json::json!(["@bg", "@fill"])
        );
    }

    #[test]
    fn self_blend_of_a_translucent_solid_is_kept() {
        // source-over stacks alpha, so a duplicated translucent layer must
        // still be composited twice to match MapLibre.
        let mut nodes = Map::new();
        nodes.insert("bg".into(), solid("#33333380"));
        let outputs = ["bg".to_string(), "bg".to_string()];
        let out = fold_blend(&mut nodes, &outputs);
        assert_eq!(out, "stack");
        assert_eq!(nodes["stack"]["layers"], serde_json::json!(["@bg", "@bg"]));
    }

    #[test]
    fn single_layer_needs_no_stack() {
        // One output composites onto nothing — return it directly, no node.
        let mut nodes = Map::new();
        nodes.insert("bg".into(), solid("#cccccc"));
        let outputs = ["bg".to_string()];
        let out = fold_blend(&mut nodes, &outputs);
        assert_eq!(out, "bg");
        assert!(!nodes.contains_key("stack"));
    }

    #[test]
    fn layers_fold_into_one_stack() {
        let mut nodes = Map::new();
        for c in ["#111111", "#222222", "#333333"] {
            nodes.insert(c.into(), solid(c));
        }
        let outputs = [
            "#111111".to_string(),
            "#222222".to_string(),
            "#333333".to_string(),
        ];
        let out = fold_blend(&mut nodes, &outputs);
        assert_eq!(out, "stack");
        assert_eq!(
            nodes["stack"]["layers"],
            serde_json::json!(["@#111111", "@#222222", "@#333333"])
        );
    }

    #[test]
    fn wide_layer_runs_fold_into_a_chain_of_narrow_stacks() {
        let mut nodes = Map::new();
        let outputs: Vec<String> = (0..18)
            .map(|i| {
                let id = format!("l{i}");
                nodes.insert(id.clone(), solid("#11223344"));
                id
            })
            .collect();
        let out = fold_blend(&mut nodes, &outputs);
        assert_eq!(out, "stack");

        // Walk the chain from the top back down and flatten it: the
        // leaves, in order, must be exactly the layers handed in.
        let mut flat: Vec<String> = Vec::new();
        let mut next = Some(out);
        while let Some(id) = next {
            let layers = nodes[&id]["layers"].as_array().unwrap().clone();
            assert!(
                layers.len() <= STACK_CHUNK,
                "no stack node should exceed the chunk width"
            );
            next = None;
            let mut here: Vec<String> = Vec::new();
            for (i, l) in layers.iter().enumerate() {
                let name = l.as_str().unwrap().trim_start_matches('@').to_string();
                if i == 0 && nodes[&name]["op"] == "stack" {
                    next = Some(name);
                } else {
                    here.push(name);
                }
            }
            // Prepend: we are walking top-down, so each chunk sits under
            // the ones already collected.
            here.append(&mut flat);
            flat = here;
        }
        assert_eq!(flat, outputs);
    }

    #[test]
    fn opaque_solid_detection() {
        let mut nodes = Map::new();
        nodes.insert("rgb".into(), solid("#abcdef"));
        nodes.insert("short".into(), solid("#abc"));
        nodes.insert("alpha_ff".into(), solid("#abcdefFF"));
        nodes.insert("alpha_80".into(), solid("#abcdef80"));
        nodes.insert(
            "dimmed".into(),
            serde_json::json!({ "op": "solid", "color": "#abcdef", "opacity": 0.5 }),
        );
        nodes.insert("not_solid".into(), serde_json::json!({ "op": "blur" }));
        assert!(is_opaque_solid(&nodes, "rgb"));
        assert!(is_opaque_solid(&nodes, "short"));
        assert!(is_opaque_solid(&nodes, "alpha_ff"));
        assert!(!is_opaque_solid(&nodes, "alpha_80"));
        assert!(!is_opaque_solid(&nodes, "dimmed"));
        assert!(!is_opaque_solid(&nodes, "not_solid"));
        assert!(!is_opaque_solid(&nodes, "missing"));
    }
}
