//! Source extraction and the per-layer source resolution helpers.

use serde_json::{Map, Value};

use crate::{ConvertError, Report};

/// Extract tiled sources. Returns the ezu `sources` object and the name of
/// the (single) vector source ezu will bind layers against.
/// The feature (non-raster) sources a recipe draws from, split by kind so
/// layers can be resolved: a `vector` layer needs a `source-layer`, a
/// `geojson` layer is itself a single feature layer.
#[derive(Default)]
pub(crate) struct Sources {
    pub(crate) vector: Vec<String>,
    pub(crate) geojson: Vec<String>,
    /// Emitted `sprite` source keys, one per sprite sheet. A style's
    /// top-level `sprite` may be a single URL (→ one `default` sheet) or an
    /// array of `{id, url}` (→ one sheet per id, with `id:icon` names). The
    /// first entry is the default for unprefixed icon names.
    pub(crate) sprites: Vec<String>,
}

impl Sources {
    /// Resolve an icon/pattern reference to `(sprite source key, icon name)`.
    /// A `sheet:icon` name selects that sheet; an unprefixed name (or an
    /// unknown prefix) falls back to the `default`/first sheet.
    pub(crate) fn resolve_icon<'a>(&self, name: &'a str) -> Option<(&str, &'a str)> {
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

pub(crate) fn convert_sources(
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
pub(crate) fn resolve_layer_source(
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
/// optional raw MapLibre `filter-expr` evaluated per feature by ezu-paint via
/// `maplibre-expr`. `source` is the ezu vector-source key (= the MapLibre
/// source name), so multiple vector sources coexist.
pub(crate) fn features_node(source: &str, source_layer: &str, filter_expr: Option<Value>) -> Value {
    let mut m = Map::new();
    m.insert("op".into(), Value::String("features".into()));
    m.insert("source".into(), Value::String(source.to_string()));
    m.insert("layer".into(), Value::String(source_layer.to_string()));
    if let Some(expr) = filter_expr {
        m.insert("filter-expr".into(), expr);
    }
    Value::Object(m)
}
