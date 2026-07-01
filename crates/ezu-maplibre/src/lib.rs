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

/// Extract tiled sources. Returns the ezu `sources` object and the name of
/// the (single) vector source ezu will bind layers against.
/// The feature (non-raster) sources a recipe draws from, split by kind so
/// layers can be resolved: a `vector` layer needs a `source-layer`, a
/// `geojson` layer is itself a single feature layer.
#[derive(Default)]
struct Sources {
    vector: Vec<String>,
    geojson: Vec<String>,
    /// Emitted `sprite` source keys, one per sprite sheet. A style's
    /// top-level `sprite` may be a single URL (→ one `default` sheet) or an
    /// array of `{id, url}` (→ one sheet per id, with `id:icon` names). The
    /// first entry is the default for unprefixed icon names.
    sprites: Vec<String>,
}

impl Sources {
    /// Resolve an icon/pattern reference to `(sprite source key, icon name)`.
    /// A `sheet:icon` name selects that sheet; an unprefixed name (or an
    /// unknown prefix) falls back to the `default`/first sheet.
    fn resolve_icon<'a>(&self, name: &'a str) -> Option<(&str, &'a str)> {
        if let Some((sheet, icon)) = name.split_once(':') {
            if let Some(key) = self.sprites.iter().find(|s| *s == sheet) {
                return Some((key, icon));
            }
        }
        let key = self
            .sprites
            .iter()
            .find(|s| *s == "default")
            .or_else(|| self.sprites.first())?;
        Some((key, name))
    }
}

fn convert_sources(
    style: &Map<String, Value>,
    report: &mut Report,
) -> Result<(Map<String, Value>, Sources), ConvertError> {
    let empty = Map::new();
    let src = style
        .get("sources")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    let mut out = Map::new();
    // Every vector/geojson source is emitted; ezu binds each under its own
    // name and `features` nodes select `(source, layer)`.
    let mut sources = Sources::default();

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
                out.insert(
                    name.clone(),
                    serde_json::json!({ "type": "mvt", "url": url }),
                );
                sources.vector.push(name.clone());
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
            "geojson" => {
                // MapLibre `data` is either an inline GeoJSON object or a URL
                // string. Emit an ezu `geojson` source carrying whichever it is;
                // the host projects lon/lat → tile-local coords per tile.
                match decl.get("data") {
                    Some(Value::String(u)) => {
                        out.insert(
                            name.clone(),
                            serde_json::json!({ "type": "geojson", "url": u }),
                        );
                        sources.geojson.push(name.clone());
                    }
                    Some(data @ (Value::Object(_) | Value::Array(_))) => {
                        out.insert(
                            name.clone(),
                            serde_json::json!({ "type": "geojson", "data": data }),
                        );
                        sources.geojson.push(name.clone());
                    }
                    _ => report.warn(format!(
                        "source `{name}`: geojson source has no usable `data` — skipped"
                    )),
                }
            }
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

    // Top-level `sprite` is a base URL (atlas `<base>.png`, index
    // `<base>.json`) or an array of `{id, url}` sheets. Emit one ezu `sprite`
    // source per sheet, keyed by its id (`default` for the single-URL form),
    // that icon / pattern layers resolve against.
    let mut emit_sprite = |key: &str, base: &str, out: &mut Map<String, Value>| {
        out.insert(
            key.to_string(),
            serde_json::json!({
                "type": "sprite",
                "image": format!("{base}.png"),
                "index": format!("{base}.json"),
            }),
        );
        sources.sprites.push(key.to_string());
    };
    match style.get("sprite") {
        Some(Value::String(base)) => emit_sprite("default", base, &mut out),
        Some(Value::Array(sheets)) => {
            for sheet in sheets {
                let id = sheet.get("id").and_then(Value::as_str);
                let url = sheet.get("url").and_then(Value::as_str);
                if let (Some(id), Some(url)) = (id, url) {
                    emit_sprite(id, url, &mut out);
                } else {
                    report.warn("sprite sheet entry missing `id`/`url` — skipped".to_string());
                }
            }
        }
        _ => {}
    }

    // A raster-only style is legal (e.g. hillshade over raster-dem); only
    // error when there's no tiled source at all.
    if sources.vector.is_empty() && sources.geojson.is_empty() && out.is_empty() {
        return Err(ConvertError::NoVectorSource);
    }
    Ok((out, sources))
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
    sources: &Sources,
    report: &mut Report,
) {
    let Some((source, source_layer)) = resolve_layer_source(id, layer, sources, report) else {
        return;
    };
    let base_filter = layer
        .get("filter")
        .and_then(|f| filter::convert(f, report, id));
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
            nodes.insert(feat_id.clone(), features_node(&source, &source_layer, filt));
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
        features_node(&source, &source_layer, base_filter),
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
fn convert_fill_extrusion(
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
    let base_filter = layer
        .get("filter")
        .and_then(|f| filter::convert(f, report, id));
    let paint = paint_of(layer);
    let color = paint.get("fill-extrusion-color");
    let opacity = paint
        .get("fill-extrusion-opacity")
        .and_then(|v| zoom::number_at(v, opts.zoom));

    let mut emit = |suffix: &str, filt: Option<Map<String, Value>>, hex: String| {
        let feat_id = format!("{id}__{suffix}_feat");
        let fill_id = format!("{id}__{suffix}_fill");
        nodes.insert(feat_id.clone(), features_node(&source, &source_layer, filt));
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

fn convert_line(
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
    let base_filter = layer
        .get("filter")
        .and_then(|f| filter::convert(f, report, id));
    let paint = paint_of(layer);

    let width = paint
        .get("line-width")
        .and_then(|v| zoom::number_at(v, opts.zoom))
        .unwrap_or(1.0)
        .max(0.1);

    // `line-pattern` replaces the solid stroke: repeat the named sprite icon
    // along each line, scaled to the stroke width (`line-stamp`).
    if let Some(pattern) = paint.get("line-pattern") {
        let opacity = paint
            .get("line-opacity")
            .and_then(|v| zoom::number_at(v, opts.zoom));
        convert_line_pattern(
            id,
            pattern,
            &source,
            &source_layer,
            base_filter,
            width,
            opacity,
            sources,
            nodes,
            outputs,
            report,
        );
        return;
    }

    let (hex, _a) = paint
        .get("line-color")
        .and_then(|v| zoom::color_at(v, opts.zoom))
        .and_then(|v| parse_color(&v))
        .unwrap_or_else(|| ("#000000".to_string(), 1.0));
    let opacity = paint
        .get("line-opacity")
        .and_then(|v| zoom::number_at(v, opts.zoom));

    // `layout.line-cap` / `-join` (MapLibre defaults: butt / miter).
    let layout = layer.get("layout").and_then(Value::as_object);
    let cap = layout
        .and_then(|l| l.get("line-cap"))
        .and_then(Value::as_str)
        .unwrap_or("butt");
    let join = layout
        .and_then(|l| l.get("line-join"))
        .and_then(Value::as_str)
        .unwrap_or("miter");

    let feat_id = format!("{id}__feat");
    let stroke_id = format!("{id}__stroke");
    nodes.insert(
        feat_id.clone(),
        features_node(&source, &source_layer, base_filter),
    );
    // Crisp `stroke` (tiny-skia) rather than a painterly brush, to match
    // MapLibre's clean vector lines.
    let mut spec = serde_json::json!({
        "op": "stroke",
        "features": format!("@{feat_id}"),
        "color": hex,
        "width-px": width,
        "cap": cap,
        "join": join,
    });
    if let Some(a) = opacity {
        spec["opacity"] = Value::from(a);
    }
    // MapLibre `line-dasharray` is in units of line width → convert to px.
    if let Some(arr) = paint.get("line-dasharray").and_then(Value::as_array) {
        let dash: Vec<Value> = arr
            .iter()
            .filter_map(Value::as_f64)
            .map(|d| Value::from(d * width))
            .collect();
        if !dash.is_empty() {
            spec["dasharray"] = Value::Array(dash);
        }
    }
    nodes.insert(stroke_id.clone(), spec);
    outputs.push(stroke_id);
}

fn convert_raster(
    id: &str,
    layer: &Map<String, Value>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    report: &mut Report,
) {
    // A raster layer references a raster source by name; ezu's `raster`
    // node picks it up by source name. We already emitted the source.
    let Some(src) = layer.get("source").and_then(Value::as_str) else {
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

/// A `circle` layer → a `circle` sprite `stamp`ed at each point feature.
/// `circle-stroke-*` is a second, larger disk stamped underneath (the ring
/// shows around the fill).
fn convert_circle(
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
    let base_filter = layer
        .get("filter")
        .and_then(|f| filter::convert(f, report, id));
    let paint = paint_of(layer);

    let num = |key: &str, default: f64| {
        paint
            .get(key)
            .and_then(|v| zoom::number_at(v, opts.zoom))
            .unwrap_or(default)
    };
    let color = |key: &str, default: &str| {
        paint
            .get(key)
            .and_then(|v| zoom::color_at(v, opts.zoom))
            .and_then(|v| parse_color(&v))
            .map(|(hex, _)| hex)
            .unwrap_or_else(|| default.to_string())
    };

    let radius = num("circle-radius", 5.0).max(0.1);
    let fill = color("circle-color", "#000000");
    let opacity = paint
        .get("circle-opacity")
        .and_then(|v| zoom::number_at(v, opts.zoom));
    let stroke_w = num("circle-stroke-width", 0.0);

    let feat_id = format!("{id}__feat");
    nodes.insert(
        feat_id.clone(),
        features_node(&source, &source_layer, base_filter),
    );

    // Stroke ring first (drawn under), then the fill disk on top.
    if stroke_w > 0.0 {
        let sc = color("circle-stroke-color", "#000000");
        emit_disk(
            id,
            "stroke",
            &feat_id,
            radius + stroke_w,
            sc,
            opacity,
            nodes,
            outputs,
        );
    }
    emit_disk(id, "fill", &feat_id, radius, fill, opacity, nodes, outputs);
}

/// Emit a `circle` sprite of pixel `radius` + a `stamp` placing it at each
/// point of `feat_id`.
#[allow(clippy::too_many_arguments)]
fn emit_disk(
    id: &str,
    suffix: &str,
    feat_id: &str,
    radius: f64,
    hex: String,
    opacity: Option<f64>,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
) {
    // 1px margin around the disk so its antialiased edge isn't clipped.
    let size = ((2.0 * radius).ceil() as i64 + 2).max(1);
    let radius_frac = radius / size as f64;
    let circle_id = format!("{id}__{suffix}_circle");
    nodes.insert(
        circle_id.clone(),
        serde_json::json!({
            "op": "circle", "kind": "sprite", "color": hex,
            "radius-frac": radius_frac, "width-px": size, "height-px": size
        }),
    );
    let stamp_id = format!("{id}__{suffix}_stamp");
    let mut spec = serde_json::json!({
        "op": "stamp", "features": format!("@{feat_id}"), "image": format!("@{circle_id}")
    });
    if let Some(a) = opacity {
        spec["opacity"] = Value::from(a);
    }
    nodes.insert(stamp_id.clone(), spec);
    outputs.push(stamp_id);
}

/// `fill-pattern` → tile the named sprite icon across the canvas and clip it
/// to the polygon coverage: `fill-solid` (opaque shape) as the clip base,
/// `icon` → `tiling` as the pattern, composed with `blend { clip: true }`
/// (source-atop keeps the pattern only inside the polygons).
#[allow(clippy::too_many_arguments)]
fn convert_fill_pattern(
    id: &str,
    pattern: &Value,
    source: &str,
    source_layer: &str,
    base_filter: Option<Map<String, Value>>,
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
        features_node(source, source_layer, base_filter),
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

/// `line-pattern` → repeat the named sprite icon along each line, fit to the
/// stroke width: `features` → `icon` → `line-stamp`.
#[allow(clippy::too_many_arguments)]
fn convert_line_pattern(
    id: &str,
    pattern: &Value,
    source: &str,
    source_layer: &str,
    base_filter: Option<Map<String, Value>>,
    width: f64,
    opacity: Option<f64>,
    sources: &Sources,
    nodes: &mut Map<String, Value>,
    outputs: &mut Vec<String>,
    report: &mut Report,
) {
    let Some(name) = pattern.as_str() else {
        report.warn(format!(
            "layer `{id}`: data-driven `line-pattern` not supported — skipped"
        ));
        return;
    };
    let Some((sprite_src, icon_name)) = sources.resolve_icon(name) else {
        report.warn(format!(
            "layer `{id}`: line-pattern `{name}` needs a `sprite`, but the style declares none — skipped"
        ));
        return;
    };
    let feat_id = format!("{id}__feat");
    nodes.insert(
        feat_id.clone(),
        features_node(source, source_layer, base_filter),
    );
    let icon_id = format!("{id}__icon");
    nodes.insert(
        icon_id.clone(),
        serde_json::json!({ "op": "icon", "sprite": format!("@{sprite_src}"), "name": icon_name }),
    );
    let out_id = format!("{id}__linepat");
    let mut spec = serde_json::json!({
        "op": "line-stamp", "features": format!("@{feat_id}"),
        "image": format!("@{icon_id}"), "width-px": width
    });
    if let Some(a) = opacity {
        spec["opacity"] = Value::from(a);
    }
    nodes.insert(out_id.clone(), spec);
    outputs.push(out_id);
}

/// A `symbol` layer's **icon** (`layout.icon-image`): place the named sprite
/// at each point feature (`features` → `icon` → `stamp`). Text labels
/// (`text-field`) are not supported yet — an icon+text layer draws the icon
/// and warns about the dropped text.
fn convert_symbol(
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

    let base_filter = layer
        .get("filter")
        .and_then(|f| filter::convert(f, report, id));
    let feat_id = format!("{id}__feat");
    nodes.insert(
        feat_id.clone(),
        features_node(&source, &source_layer, base_filter),
    );
    let icon_id = format!("{id}__icon");
    nodes.insert(
        icon_id.clone(),
        serde_json::json!({ "op": "icon", "sprite": format!("@{sprite_src}"), "name": sprite_icon }),
    );

    let size = layout
        .and_then(|l| l.get("icon-size"))
        .and_then(|v| zoom::number_at(v, opts.zoom))
        .unwrap_or(1.0);
    let rotate = layout
        .and_then(|l| l.get("icon-rotate"))
        .and_then(|v| zoom::number_at(v, opts.zoom))
        .unwrap_or(0.0);
    let opacity = paint_of(layer)
        .get("icon-opacity")
        .and_then(|v| zoom::number_at(v, opts.zoom));

    let stamp_id = format!("{id}__stamp");
    let mut spec = serde_json::json!({
        "op": "stamp", "features": format!("@{feat_id}"), "image": format!("@{icon_id}")
    });
    if size != 1.0 {
        spec["scale"] = Value::from(size);
    }
    if rotate != 0.0 {
        spec["rotation-deg"] = Value::from(rotate);
    }
    if let Some(a) = opacity {
        spec["opacity"] = Value::from(a);
    }
    nodes.insert(stamp_id.clone(), spec);
    outputs.push(stamp_id);
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

/// Resolve a data layer to its `(ezu source key, feature-layer name)`.
///
/// - A **vector** source layer takes its feature-layer name from
///   `source-layer` (required — MapLibre vector layers always name one).
/// - A **geojson** source is itself a single feature layer, bound under
///   `<source>.<source>`, so its layer name *is* the source name and no
///   `source-layer` is expected.
///
/// Warns + returns `None` for a missing source, an unconverted source, or
/// a vector layer lacking `source-layer`.
fn resolve_layer_source(
    id: &str,
    layer: &Map<String, Value>,
    sources: &Sources,
    report: &mut Report,
) -> Option<(String, String)> {
    let Some(s) = layer.get("source").and_then(Value::as_str) else {
        report.warn(format!("layer `{id}`: no source — skipped"));
        return None;
    };
    if sources.vector.iter().any(|v| v == s) {
        let Some(sl) = layer.get("source-layer").and_then(Value::as_str) else {
            report.warn(format!(
                "layer `{id}`: vector layer without `source-layer` — skipped"
            ));
            return None;
        };
        Some((s.to_string(), sl.to_string()))
    } else if sources.geojson.iter().any(|g| g == s) {
        Some((s.to_string(), s.to_string()))
    } else {
        report.warn(format!(
            "layer `{id}`: source `{s}` is not a converted feature source — skipped"
        ));
        None
    }
}

/// A `features` source node selecting `(source, source-layer)`, with an
/// optional (already-converted) filter. `source` is the ezu vector-source
/// key (= the MapLibre source name), so multiple vector sources coexist.
fn features_node(source: &str, source_layer: &str, filter: Option<Map<String, Value>>) -> Value {
    let mut m = Map::new();
    m.insert("op".into(), Value::String("features".into()));
    m.insert("source".into(), Value::String(source.to_string()));
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

fn paint_of(layer: &Map<String, Value>) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    layer
        .get("paint")
        .and_then(Value::as_object)
        .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}
