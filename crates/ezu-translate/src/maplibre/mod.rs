//! Convert [MapLibre GL styles] into **ezu recipes** — the node-DAG
//! [`Document`](ezu-style) JSON that ezu renders on the CPU.
//!
//! MapLibre is an ordered list of layers whose paint/layout properties
//! are computed per feature and per (fractional) zoom via *expressions*.
//! ezu is a typed node DAG whose ops are styled uniformly. The two models
//! differ deeply, so this converter targets the tractable subset first
//! (see the crate README / `project_maplibre_conversion` notes):
//!
//! - **Layer list → blend chain.** The painter's algorithm (each layer
//!   drawn over the last) maps to an ezu `blend` fold.
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
//!   `features` node's `min-zoom`/`max-zoom` render-time gate.
//!
//! - **sprites**: a top-level `sprite` (single URL or `[{id, url}]`
//!   sheets) becomes `sprite` source(s); `symbol` **icons**,
//!   `fill-pattern`, and `line-pattern` wire through `icon` (crop) +
//!   `stamp` / `tiling` / `line-stamp`.
//! - **`symbol` text** (point placement): `text-field` and its paint /
//!   layout properties lower to the `text` node. A `text-font` stack
//!   with no [`ConvertOptions::fonts`] mapping is served from the
//!   style's own top-level `glyphs` endpoint as an SDF `glyphs` source
//!   — zero configuration; an explicit font-URL mapping wins and gives
//!   higher-fidelity outline rendering.
//!
//! What is *not* handled yet is reported in [`Report::warnings`] rather
//! than failing the conversion: `symbol-placement: line`, text collision
//! / variable anchors, and expression operators outside the set above.
//! Inline/remote `geojson` sources *are* converted (the host projects
//! them into each tile).
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
    convert_hillshade, convert_line, convert_raster, convert_symbol,
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
    /// Emitted `pad` — the buffer around the tile where blurs and
    /// overflowing geometry land before the crop.
    pub pad: u32,
    /// How to treat `layout.visibility: "none"` layers. `false` (default)
    /// drops them. `true` keeps their nodes in the recipe but gates each
    /// behind a `switch` that defaults to a transparent branch — so the
    /// layer is off yet present, and flipping the switch's `select` to `b`
    /// turns it on (a build-time toggle, since `switch` resolves at build).
    pub keep_hidden: bool,
    /// MapLibre fontstack entry name → font file URL, used to lower
    /// `symbol` text (`text-font`) to ezu `font` sources (CLI:
    /// repeatable `--font NAME=URL`). Optional: a stack with no mapped
    /// entry falls back to the style's top-level `glyphs` endpoint as
    /// an SDF `glyphs` source (MapLibre's own rendering path); a
    /// mapping wins where present and renders from the real font file.
    /// No mapping *and* no `glyphs` skips the text with a warning.
    pub fonts: std::collections::HashMap<String, String>,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            tile_size: 512,
            pad: 64,
            keep_hidden: false,
            fonts: std::collections::HashMap::new(),
        }
    }
}

/// A layer's `(minzoom, maxzoom)` render-time gate, threaded onto its
/// `features` node as `min-zoom`/`max-zoom`. `None` where the layer omits
/// the bound.
pub(crate) type ZoomRange = (Option<u8>, Option<u8>);

/// Read a layer's `minzoom`/`maxzoom` (JSON numbers) as `u8`, clamped to
/// ezu's `0..=24` zoom range.
fn layer_zoom_range(layer: &Map<String, Value>) -> ZoomRange {
    let read = |key| {
        layer
            .get(key)
            .and_then(Value::as_f64)
            .map(|z| z.round().clamp(0.0, 24.0) as u8)
    };
    (read("minzoom"), read("maxzoom"))
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
        // gate (`min-zoom`/`max-zoom`), computed once per layer.
        let zoom_range = layer_zoom_range(layer);
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
            "heatmap" => convert_heatmap(
                id,
                layer,
                &mut nodes,
                &mut outputs,
                zoom_range,
                &sources,
                &mut report,
            ),
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
    doc.insert("pad".into(), Value::from(opts.pad));
    doc.insert("sources".into(), Value::Object(source_defs));
    doc.insert("nodes".into(), Value::Object(nodes));
    doc.insert("output".into(), Value::String(output));

    Ok((Value::Object(doc), report))
}

/// Fold the ordered output node ids into a `blend` chain (painter's
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
            let mut cur = first.clone();
            for (i, over) in rest.iter().enumerate() {
                let bid = format!("blend_{i}");
                nodes.insert(
                    bid.clone(),
                    serde_json::json!({ "op": "blend", "base": format!("@{cur}"), "over": format!("@{over}") }),
                );
                cur = bid;
            }
            cur
        }
    }
}
