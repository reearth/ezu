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
//! What is *not* handled yet is reported in [`Report::warnings`] rather
//! than failing the conversion: `symbol` (text) layers, per-feature
//! data-driven paint (other than the `match`-bucket case), inline GeoJSON
//! sources, `line-dasharray`, and expression operators outside the set
//! above.
//!
//! [MapLibre GL styles]: https://maplibre.org/maplibre-style-spec/
//! [`Document`]: https://docs.rs/ezu-style

use serde_json::{Map, Value};

mod color;
mod filter;
mod zoom;

use color::parse_color;

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
    /// Brush hardness for `line` layers (0..1). 1.0 is crispest; MapLibre
    /// lines are hard vector strokes, so default high.
    pub line_hardness: f64,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            zoom: None,
            tile_size: 512,
            pad: 64,
            line_hardness: 0.9,
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
    let (sources, vector_source) = convert_sources(style, &mut report)?;

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
        // Honour `layout.visibility: "none"`.
        if layer
            .get("layout")
            .and_then(|l| l.get("visibility"))
            .and_then(Value::as_str)
            == Some("none")
        {
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
        match ty {
            "background" => convert_background(id, layer, &mut nodes, &mut outputs, opts),
            "fill" => convert_fill(id, layer, &mut nodes, &mut outputs, opts, &mut report),
            "line" => convert_line(id, layer, &mut nodes, &mut outputs, opts, &mut report),
            "raster" => convert_raster(
                id,
                layer,
                &vector_source,
                &mut nodes,
                &mut outputs,
                &mut report,
            ),
            "hillshade" => {
                convert_hillshade(id, layer, &mut nodes, &mut outputs, opts, &mut report)
            }
            "symbol" => report.warn(format!(
                "layer `{id}`: `symbol` (text/icon labels) not supported yet — skipped"
            )),
            other => report.warn(format!(
                "layer `{id}`: type `{other}` not supported — skipped"
            )),
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
    doc.insert("sources".into(), Value::Object(sources));
    doc.insert("nodes".into(), Value::Object(nodes));
    doc.insert("output".into(), Value::String(output));

    Ok((Value::Object(doc), report))
}

/// Extract tiled sources. Returns the ezu `sources` object and the name of
/// the (single) vector source ezu will bind layers against.
fn convert_sources(
    style: &Map<String, Value>,
    report: &mut Report,
) -> Result<(Map<String, Value>, String), ConvertError> {
    let empty = Map::new();
    let src = style
        .get("sources")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    let mut out = Map::new();
    let mut vector_source: Option<String> = None;

    for (name, decl) in src {
        let Some(decl) = decl.as_object() else {
            continue;
        };
        let ty = decl.get("type").and_then(Value::as_str).unwrap_or("");
        // MapLibre gives either `url` (TileJSON) or `tiles` (XYZ templates).
        let url = decl
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                decl.get("tiles")
                    .and_then(Value::as_array)
                    .and_then(|t| t.first())
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        match ty {
            "vector" => {
                let Some(url) = url else {
                    report.warn(format!(
                        "source `{name}`: vector source has no url/tiles — skipped"
                    ));
                    continue;
                };
                if vector_source.is_some() {
                    report.warn(format!(
                        "source `{name}`: ezu supports one vector source per style — ignored (using the first)"
                    ));
                    continue;
                }
                out.insert(
                    name.clone(),
                    serde_json::json!({ "type": "mvt", "url": url }),
                );
                vector_source = Some(name.clone());
            }
            "raster" => {
                if let Some(url) = url {
                    out.insert(
                        name.clone(),
                        serde_json::json!({ "type": "raster", "url": url }),
                    );
                } else {
                    report.warn(format!(
                        "source `{name}`: raster source has no url/tiles — skipped"
                    ));
                }
            }
            "geojson" => report.warn(format!(
                "source `{name}`: inline/remote GeoJSON sources not supported yet — skipped"
            )),
            "raster-dem" => {
                if let Some(url) = url {
                    // Encoding hint: MapLibre `encoding` maps to ezu's.
                    let enc = decl
                        .get("encoding")
                        .and_then(Value::as_str)
                        .unwrap_or("mapbox");
                    let tile_size = decl.get("tileSize").and_then(Value::as_u64).unwrap_or(512);
                    // `neighbor-fetch` stitches the 3×3 tile neighbourhood so
                    // hillshade slopes stay correct up to the tile edge.
                    let mut dem = serde_json::json!({
                        "type": "dem", "url": url, "encoding": enc,
                        "tile-size": tile_size, "neighbor-fetch": true
                    });
                    if let Some(mz) = decl.get("maxzoom").and_then(Value::as_u64) {
                        dem["max-zoom"] = Value::from(mz);
                    }
                    out.insert(name.clone(), dem);
                } else {
                    report.warn(format!(
                        "source `{name}`: raster-dem has no url/tiles — skipped"
                    ));
                }
            }
            other => report.warn(format!(
                "source `{name}`: type `{other}` not supported — skipped"
            )),
        }
    }

    let vector_source = vector_source
        .ok_or(ConvertError::NoVectorSource)
        .or_else(|e| {
            // A raster-only style is legal; only error if there are also no
            // raster/dem sources to render.
            if out.is_empty() {
                Err(e)
            } else {
                Ok(String::new())
            }
        })?;

    Ok((out, vector_source))
}

fn convert_background(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    opts: &ConvertOptions,
) {
    let paint = paint_of(layer);
    let (hex, _a) = paint
        .get("background-color")
        .and_then(|v| zoom::color_at(v, opts.zoom))
        .and_then(|v| parse_color(&v))
        .unwrap_or_else(|| ("#000000".to_string(), 1.0));
    let nid = format!("{id}__bg");
    nodes.insert(
        nid.clone(),
        serde_json::json!({ "op": "solid", "color": hex }),
    );
    outputs.push(nid);
}

fn convert_fill(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    opts: &ConvertOptions,
    report: &mut Report,
) {
    let Some(source_layer) = layer.get("source-layer").and_then(Value::as_str) else {
        report.warn(format!("layer `{id}`: fill without source-layer — skipped"));
        return;
    };
    let base_filter = layer
        .get("filter")
        .and_then(|f| filter::convert(f, report, id));
    let paint = paint_of(layer);
    let fill_color = paint.get("fill-color");
    let opacity = paint
        .get("fill-opacity")
        .and_then(|v| zoom::number_at(v, opts.zoom));

    // `fill-color: ["match", ["get", prop], vals, color, ..., fallback]`
    // → one filtered fill-solid per bucket, plus a fallback underneath.
    if let Some(buckets) = fill_color.and_then(color::match_buckets) {
        // Fallback first (drawn underneath the specific buckets).
        let mut emit = |suffix: &str, filt: Option<Map<String, Value>>, hex: String| {
            let feat_id = format!("{id}__{suffix}_feat");
            let fill_id = format!("{id}__{suffix}_fill");
            nodes.insert(feat_id.clone(), features_node(source_layer, filt));
            let mut spec = serde_json::json!({ "op": "fill-solid", "features": format!("@{feat_id}"), "fill": hex });
            if let Some(a) = opacity {
                spec["fill-alpha"] = Value::from(a);
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
    nodes.insert(feat_id.clone(), features_node(source_layer, base_filter));
    let mut spec =
        serde_json::json!({ "op": "fill-solid", "features": format!("@{feat_id}"), "fill": hex });
    if let Some(a) = opacity {
        spec["fill-alpha"] = Value::from(a);
    }
    nodes.insert(fill_id.clone(), spec);
    outputs.push(fill_id);
}

fn convert_line(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    opts: &ConvertOptions,
    report: &mut Report,
) {
    let Some(source_layer) = layer.get("source-layer").and_then(Value::as_str) else {
        report.warn(format!("layer `{id}`: line without source-layer — skipped"));
        return;
    };
    let base_filter = layer
        .get("filter")
        .and_then(|f| filter::convert(f, report, id));
    let paint = paint_of(layer);

    let width = paint
        .get("line-width")
        .and_then(|v| zoom::number_at(v, opts.zoom))
        .unwrap_or(1.0)
        .max(0.1);
    let (hex, _a) = paint
        .get("line-color")
        .and_then(|v| zoom::color_at(v, opts.zoom))
        .and_then(|v| parse_color(&v))
        .unwrap_or_else(|| ("#000000".to_string(), 1.0));
    let opacity = paint
        .get("line-opacity")
        .and_then(|v| zoom::number_at(v, opts.zoom));

    if paint.contains_key("line-dasharray") {
        report.warn(format!(
            "layer `{id}`: line-dasharray not supported — drawn solid"
        ));
    }

    let feat_id = format!("{id}__feat");
    let brush_id = format!("{id}__brush");
    let line_id = format!("{id}__line");
    nodes.insert(feat_id.clone(), features_node(source_layer, base_filter));
    nodes.insert(
        brush_id.clone(),
        serde_json::json!({ "op": "brush-solid", "width-px": width, "hardness": opts.line_hardness, "color": hex }),
    );
    let mut spec = serde_json::json!({
        "op": "line",
        "features": format!("@{feat_id}"),
        "brush": format!("@{brush_id}"),
        "color": hex,
        "radius-px": width * 0.5,
    });
    if let Some(a) = opacity {
        spec["opacity"] = Value::from(a);
    }
    nodes.insert(line_id.clone(), spec);
    outputs.push(line_id);
}

fn convert_raster(
    id: &str,
    _layer: &Map<String, Value>,
    _vector_source: &str,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    report: &mut Report,
) {
    // A raster layer references a raster source by name; ezu's `raster`
    // node picks it up by source name. We already emitted the source.
    let src = _layer.get("source").and_then(Value::as_str);
    let Some(src) = src else {
        report.warn(format!("layer `{id}`: raster without source — skipped"));
        return;
    };
    let nid = format!("{id}__raster");
    nodes.insert(
        nid.clone(),
        serde_json::json!({ "op": "raster", "source": src }),
    );
    outputs.push(nid);
}

/// A `hillshade` layer over a `raster-dem` source → an ezu `dem` node
/// feeding a `hillshade` node. ezu already has the whole terrain stack
/// (`dem` / `hillshade` / `slope` / `color-ramp`); this just wires the
/// MapLibre paint props onto it.
fn convert_hillshade(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    opts: &ConvertOptions,
    report: &mut Report,
) {
    let Some(src) = layer.get("source").and_then(Value::as_str) else {
        report.warn(format!("layer `{id}`: hillshade without source — skipped"));
        return;
    };
    let paint = paint_of(layer);
    // MapLibre defaults: illumination-direction 335°, exaggeration 0.5.
    let azimuth = paint
        .get("hillshade-illumination-direction")
        .and_then(|v| zoom::number_at(v, opts.zoom))
        .unwrap_or(335.0);
    let exaggeration = paint
        .get("hillshade-exaggeration")
        .and_then(|v| zoom::number_at(v, opts.zoom))
        .unwrap_or(0.5);

    let dem_id = format!("{id}__dem");
    let hs_id = format!("{id}__hillshade");
    nodes.insert(
        dem_id.clone(),
        serde_json::json!({ "op": "dem", "source": src }),
    );
    nodes.insert(
        hs_id.clone(),
        serde_json::json!({
            "op": "hillshade",
            "field": format!("@{dem_id}"),
            "azimuth-deg": azimuth,
            "altitude-deg": 45,
            "exaggeration": exaggeration,
            // `relief` leaves flat ground white and only darkens slopes,
            // matching MapLibre's hillshade look over a light background
            // (vs `shade`, which greys flat ground too).
            "mode": "relief"
        }),
    );
    outputs.push(hs_id);
}

/// A `features` source node selecting `source-layer`, with an optional
/// (already-converted) filter.
fn features_node(source_layer: &str, filter: Option<Map<String, Value>>) -> Value {
    let mut m = Map::new();
    m.insert("op".into(), Value::String("features".into()));
    m.insert("layer".into(), Value::String(source_layer.to_string()));
    if let Some(f) = filter {
        if !f.is_empty() {
            m.insert("filter".into(), Value::Object(f));
        }
    }
    Value::Object(m)
}

/// Fold the ordered output node ids into a `blend` chain (painter's
/// algorithm) and return the id of the final node.
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

fn paint_of(layer: &Map<String, Value>) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    layer
        .get("paint")
        .and_then(Value::as_object)
        .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}
