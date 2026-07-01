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
//! - **zoom functions** (legacy `stops` and `interpolate`) are baked to a
//!   constant at [`ConvertOptions::zoom`] when supplied — ezu renders one
//!   integer zoom per tile, so a per-zoom bake is exact for that tile.
//!
//! - **sprites**: a top-level `sprite` (single URL or `[{id, url}]`
//!   sheets) becomes `sprite` source(s); `symbol` **icons**,
//!   `fill-pattern`, and `line-pattern` wire through `icon` (crop) +
//!   `stamp` / `tiling` / `line-stamp`.
//!
//! What is *not* handled yet is reported in [`Report::warnings`] rather
//! than failing the conversion: `symbol` **text** labels, per-feature
//! data-driven paint (other than the `match`-bucket case), and expression
//! operators outside the set above. Inline/remote `geojson` sources *are*
//! converted (the host projects them into each tile).
//!
//! [MapLibre GL styles]: https://maplibre.org/maplibre-style-spec/
//! [`Document`]: https://docs.rs/ezu-style

use serde_json::{Map, Value};

pub(crate) mod color;
pub(crate) mod filter;
pub(crate) mod layers;
pub(crate) mod sources;
pub(crate) mod zoom;

use layers::{
    convert_background, convert_circle, convert_fill, convert_fill_extrusion, convert_hillshade,
    convert_line, convert_raster, convert_symbol,
};
use sources::convert_sources;

/// Knobs controlling how a MapLibre style is lowered to an ezu recipe.
#[derive(Debug, Clone)]
pub struct ConvertOptions {
    /// Zoom level at which to bake zoom-dependent property functions
    /// (legacy `stops`, `interpolate`). `None` uses each function's base
    /// value (first stop). Because ezu renders a single integer zoom per
    /// tile, baking at the tile's zoom reproduces MapLibre exactly there.
    pub zoom: Option<f64>,
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
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            zoom: None,
            tile_size: 512,
            pad: 64,
            keep_hidden: false,
        }
    }
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
    let (source_defs, sources) = convert_sources(style, &mut report)?;

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
        // Honour the layer's zoom range at the baked zoom (MapLibre shows a
        // layer for `minzoom <= z < maxzoom`). ezu renders one zoom per
        // tile, so a layer outside the range simply isn't emitted.
        if let Some(z) = opts.zoom {
            let below = layer
                .get("minzoom")
                .and_then(Value::as_f64)
                .is_some_and(|mz| z < mz);
            let above = layer
                .get("maxzoom")
                .and_then(Value::as_f64)
                .is_some_and(|mz| z >= mz);
            if below || above {
                continue;
            }
        }
        let ty = layer.get("type").and_then(Value::as_str).unwrap_or("");
        let out_start = outputs.len();
        match ty {
            "background" => convert_background(id, layer, &mut nodes, &mut outputs, opts),
            "fill" => convert_fill(
                id,
                layer,
                &mut nodes,
                &mut outputs,
                opts,
                &sources,
                &mut report,
            ),
            "line" => convert_line(
                id,
                layer,
                &mut nodes,
                &mut outputs,
                opts,
                &sources,
                &mut report,
            ),
            "raster" => convert_raster(id, layer, &mut nodes, &mut outputs, &mut report),
            "circle" => convert_circle(
                id,
                layer,
                &mut nodes,
                &mut outputs,
                opts,
                &sources,
                &mut report,
            ),
            "hillshade" => {
                convert_hillshade(id, layer, &mut nodes, &mut outputs, opts, &mut report)
            }
            "fill-extrusion" => convert_fill_extrusion(
                id,
                layer,
                &mut nodes,
                &mut outputs,
                opts,
                &sources,
                &mut report,
            ),
            "symbol" => convert_symbol(
                id,
                layer,
                &mut nodes,
                &mut outputs,
                opts,
                &sources,
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
