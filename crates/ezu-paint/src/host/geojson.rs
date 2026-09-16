//! Document-scoped GeoJSON sources: resolved once, projected per tile.
//!
//! A `geojson` source names a WGS84 lon/lat document, either inline as
//! `data` or by `url`. Unlike `mvt`, `pmtiles` and `dem`, nothing about it
//! is tile-addressed: one document covers the whole world, and every tile
//! that draws from it re-projects the same features into its own frame.
//!
//! That split is what this module encodes. [`GeoJsonSources`] resolves each
//! source to parsed JSON **once per document** — which is where a `url` is
//! fetched — and [`bind_geojson_sources`] projects that JSON into one tile,
//! binding each source as a single feature layer named `<source>.<source>`
//! (the name a `features` node spells as `source: "s", layer: "s"`). A
//! graph that reads neighbour tiles, as cross-tile label collision does,
//! gets those projected too.
//!
//! Every host renders geojson through these two calls, so inline and remote
//! documents behave the same whether the tile comes from `ezu tile`, the
//! dev server, `ezu-compare`, or the browser.

use std::sync::Arc;

use ezu_features::FeatureLayer;
use ezu_style::{Document, SourceDecl};
use serde_json::Value;

use crate::host::{requested_neighbor_offsets, TileLoader};

/// The MVT integer extent inline GeoJSON is projected onto. Features
/// arrive in the same coordinate space as a decoded vector tile, so nodes
/// downstream cannot tell the two apart.
const EXTENT: u32 = 4096;

/// A document's `geojson` sources, each resolved to a parsed GeoJSON
/// document. Built once per style and reused for every tile, since the
/// document is the same for all of them.
///
/// The document's source order is preserved, so binding is deterministic.
#[derive(Debug, Default, Clone)]
pub struct GeoJsonSources {
    sources: Vec<(String, Arc<Value>)>,
}

impl GeoJsonSources {
    /// The sources a document carries inline, with no I/O. This is what a
    /// host that fetches remote documents itself starts from — the browser
    /// host, where `url` is JS's job and arrives through [`Self::insert`].
    pub fn inline(doc: &Document) -> Self {
        let sources =
            doc.sources
                .iter()
                .filter_map(|(name, decl)| match decl {
                    SourceDecl::GeoJson(g) => inline_data(g.data.as_ref())
                        .map(|data| (name.clone(), Arc::new(data.clone()))),
                    _ => None,
                })
                .collect();
        Self { sources }
    }

    /// Every `geojson` source in `doc`, with each `url` read and parsed.
    ///
    /// `data` wins over `url` when a source declares both. A `url` goes
    /// through the same resolver as any other document asset, so
    /// `http(s)://`, `file:` (against `base_dir`) and `data:` all work; a
    /// `file:` document is how a style keeps its features beside it rather
    /// than inside it.
    ///
    /// The read happens here, once, rather than per tile — a pyramid run
    /// fetches a remote document exactly as many times as a single tile
    /// does.
    #[cfg(feature = "http")]
    pub async fn resolve(doc: &Document, base_dir: &std::path::Path) -> Result<Self, String> {
        let mut sources = Vec::new();
        for (name, decl) in &doc.sources {
            let SourceDecl::GeoJson(g) = decl else {
                continue;
            };
            if let Some(data) = inline_data(g.data.as_ref()) {
                sources.push((name.clone(), Arc::new(data.clone())));
                continue;
            }
            // A string `data` is MapLibre's other spelling of `url`, and a
            // style converted before that was normalised can still carry it.
            let url = g
                .url
                .as_deref()
                .or_else(|| g.data.as_ref().and_then(Value::as_str));
            let Some(url) = url else {
                return Err(format!(
                    "geojson `{name}`: declares neither inline `data` nor a `url`"
                ));
            };
            let bytes = super::read_asset_bytes(url, base_dir)
                .await
                .map_err(|e| format!("geojson `{name}`: {e}"))?;
            let data: Value = serde_json::from_slice(&bytes)
                .map_err(|e| format!("geojson `{name}` parse: {e}"))?;
            sources.push((name.clone(), Arc::new(data)));
        }
        Ok(Self { sources })
    }

    /// Add a source resolved by the host itself, replacing any entry of the
    /// same name. Ordering follows first insertion.
    pub fn insert(&mut self, name: impl Into<String>, data: Arc<Value>) {
        let name = name.into();
        match self.sources.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = data,
            None => self.sources.push((name, data)),
        }
    }

    /// Whether `name` is already resolved — a host that binds some sources
    /// itself uses this to leave those alone.
    pub fn contains(&self, name: &str) -> bool {
        self.sources.iter().any(|(n, _)| n == name)
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// The resolved source names, in the document's order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.sources.iter().map(|(n, _)| n.as_str())
    }

    /// Drop every source whose name `keep` rejects — for a host that has
    /// already bound some of them by another route.
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.sources.retain(|(name, _)| keep(name));
    }
}

/// An inline `data` value, if it is a GeoJSON document rather than absent
/// or a URL string. MapLibre overloads `data` to carry either, and a
/// converted style can still hold the string form.
fn inline_data(data: Option<&Value>) -> Option<&Value> {
    data.filter(|d| d.is_object() || d.is_array())
}

/// Project every source in `sources` into `loader`'s tile and bind it as a
/// feature layer, plus into whichever neighbour tiles `requested` asks for.
///
/// `requested` is [`ezu_graph::Graph::asset_inputs`]: a node that reads a
/// neighbour names it, so only the neighbours some node actually reads are
/// projected. Usually that is none.
///
/// A document that fails to project is an error rather than an empty
/// layer. Bad GeoJSON is a fault in the style, and a blank tile is the one
/// symptom that never points at it.
pub fn bind_geojson_sources(
    loader: &mut TileLoader<'_>,
    sources: &GeoJsonSources,
    requested: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    for (name, data) in &sources.sources {
        bind_geojson(loader, name, data, 0, 0)?;
        for (dx, dy) in requested_neighbor_offsets(requested, name) {
            bind_geojson(loader, name, data, dx, dy)?;
        }
    }
    Ok(())
}

/// Project `data` into the tile `(dx, dy)` away from the loader's own
/// (`(0, 0)` being that tile) and bind it under `<name>.<name>`, or
/// `<name>.<name>@dx,dy` for a neighbour. `x` wraps at the antimeridian;
/// a `y` past a pole has no neighbour and is skipped.
pub fn bind_geojson(
    loader: &mut TileLoader<'_>,
    name: &str,
    data: &Value,
    dx: i32,
    dy: i32,
) -> Result<(), String> {
    let tile = loader.tile();
    let world = 1i64 << tile.z;
    let ny = tile.y as i64 + dy as i64;
    if ny < 0 || ny >= world {
        return Ok(());
    }
    let nx = (tile.x as i64 + dx as i64).rem_euclid(world) as u32;
    let features = ezu_features::geojson::decode_projected(data, tile.z, nx, ny as u32, EXTENT)
        .map_err(|e| format!("geojson `{name}`: {e}"))?;
    let base = format!("{name}.{name}");
    loader.bind_features(
        ezu_graph::neighbor_binding(&base, dx, dy),
        FeatureLayer {
            name: name.to_string(),
            extent: EXTENT,
            features,
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::BrushBankLoader;
    use ezu_graph::{Asset, AssetLoader, TileId};
    use std::collections::BTreeSet;

    /// A one-point FeatureCollection at `(lon, lat)`, as a style would
    /// spell it inline.
    fn doc_with(source: &str, data: &str) -> Document {
        Document::from_json(&format!(
            r#"{{
                "name": "t",
                "sources": {{ "{source}": {{ "type": "geojson", "data": {data} }} }},
                "nodes": {{ "f": {{ "op": "features", "source": "{source}", "layer": "{source}" }} }},
                "output": "@f"
            }}"#
        ))
        .expect("style parses")
    }

    const ONE_POINT: &str = r#"{
        "type": "FeatureCollection",
        "features": [
            { "type": "Feature", "properties": {},
              "geometry": { "type": "Point", "coordinates": [0.0, 0.0] } }
        ]
    }"#;

    fn requested(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    /// How many features are bound under `name`, or `None` when nothing is.
    fn features_at(loader: &TileLoader<'_>, name: &str) -> Option<usize> {
        match loader.load(name) {
            Ok(Asset::Features(opaque)) => opaque
                .downcast_ref::<crate::render::SharedLayer>()
                .map(|shared| shared.layer.features.len()),
            _ => None,
        }
    }

    #[test]
    fn inline_data_binds_under_the_source_name_twice_over() {
        let doc = doc_with("pins", ONE_POINT);
        let sources = GeoJsonSources::inline(&doc);
        let base = BrushBankLoader::new();
        let mut loader = TileLoader::new(&base, TileId { z: 0, x: 0, y: 0 });

        bind_geojson_sources(&mut loader, &sources, &requested(&[])).unwrap();

        // A geojson source is its own single layer, so the binding name
        // doubles the source name — that is what `features` spells.
        assert_eq!(features_at(&loader, "pins.pins"), Some(1));
    }

    #[test]
    fn a_url_shaped_data_string_is_not_inline() {
        // MapLibre overloads `data` to carry a URL. That is not a document,
        // so there is nothing to bind without reading it first.
        let doc = doc_with("pins", r#""https://example.com/pins.geojson""#);
        assert!(GeoJsonSources::inline(&doc).is_empty());
    }

    #[test]
    fn only_the_neighbours_the_graph_asks_for_are_projected() {
        let doc = doc_with("pins", ONE_POINT);
        let sources = GeoJsonSources::inline(&doc);
        let base = BrushBankLoader::new();
        let mut loader = TileLoader::new(&base, TileId { z: 2, x: 1, y: 1 });

        bind_geojson_sources(&mut loader, &sources, &requested(&["pins.pins@1,0"])).unwrap();

        assert_eq!(features_at(&loader, "pins.pins"), Some(1));
        assert!(features_at(&loader, "pins.pins@1,0").is_some());
        // Never asked for, never projected.
        assert!(features_at(&loader, "pins.pins@0,1").is_none());
    }

    #[test]
    fn neighbours_wrap_in_x_and_stop_at_the_poles() {
        let doc = doc_with("pins", ONE_POINT);
        let sources = GeoJsonSources::inline(&doc);
        let base = BrushBankLoader::new();
        // Top-left tile of z=1: west of it is the antimeridian, north of it
        // is nothing at all.
        let mut loader = TileLoader::new(&base, TileId { z: 1, x: 0, y: 0 });

        bind_geojson_sources(
            &mut loader,
            &sources,
            &requested(&["pins.pins@-1,0", "pins.pins@0,-1"]),
        )
        .unwrap();

        // x wraps to the far side of the world and still binds…
        assert!(features_at(&loader, "pins.pins@-1,0").is_some());
        // …while a row above the top one does not exist to bind.
        assert!(features_at(&loader, "pins.pins@0,-1").is_none());
    }

    #[test]
    fn malformed_geojson_is_an_error_not_an_empty_layer() {
        let doc = doc_with("pins", r#"{ "type": "NotAGeoJsonThing" }"#);
        let sources = GeoJsonSources::inline(&doc);
        let base = BrushBankLoader::new();
        let mut loader = TileLoader::new(&base, TileId { z: 0, x: 0, y: 0 });

        let err = bind_geojson_sources(&mut loader, &sources, &requested(&[])).unwrap_err();
        assert!(err.contains("pins"), "error should name the source: {err}");
    }

    #[test]
    fn retain_drops_what_the_host_bound_itself() {
        let doc = doc_with("pins", ONE_POINT);
        let mut sources = GeoJsonSources::inline(&doc);
        assert!(sources.contains("pins"));
        sources.retain(|name| name != "pins");
        assert!(sources.is_empty());
    }

    #[cfg(feature = "http")]
    #[tokio::test]
    async fn a_url_source_is_read_and_behaves_like_an_inline_one() {
        use base64::Engine;

        // `data:` goes through the same resolver as `http(s)://` and `file:`,
        // so this covers the `url` path without reaching the network.
        let encoded = base64::engine::general_purpose::STANDARD.encode(ONE_POINT);
        let doc = Document::from_json(&format!(
            r#"{{
                "name": "t",
                "sources": {{ "pins": {{
                    "type": "geojson",
                    "url": "data:application/geo+json;base64,{encoded}"
                }} }},
                "nodes": {{ "f": {{ "op": "features", "source": "pins", "layer": "pins" }} }},
                "output": "@f"
            }}"#
        ))
        .expect("style parses");

        let sources = GeoJsonSources::resolve(&doc, std::path::Path::new("."))
            .await
            .expect("data: url resolves");
        let base = BrushBankLoader::new();
        let mut loader = TileLoader::new(&base, TileId { z: 0, x: 0, y: 0 });
        bind_geojson_sources(&mut loader, &sources, &requested(&[])).unwrap();

        assert_eq!(features_at(&loader, "pins.pins"), Some(1));
    }

    #[cfg(feature = "http")]
    #[tokio::test]
    async fn a_source_with_neither_data_nor_url_is_rejected() {
        let doc = Document::from_json(
            r#"{
                "name": "t",
                "sources": { "pins": { "type": "geojson" } },
                "nodes": { "f": { "op": "features", "source": "pins", "layer": "pins" } },
                "output": "@f"
            }"#,
        )
        .expect("style parses");

        let err = GeoJsonSources::resolve(&doc, std::path::Path::new("."))
            .await
            .expect_err("nothing to read");
        assert!(err.contains("pins"), "error should name the source: {err}");
    }
}
