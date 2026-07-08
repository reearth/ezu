//! WebAssembly bindings for the ezu painterly map renderer (Style).
//!
//! The JS side owns all I/O (HTTP, PMTiles, asset fetching). This crate
//! exposes a stateful [`Renderer`] that holds a parsed style document,
//! its built graph, an in-memory brush bank, and a per-tile binding
//! buffer that mirrors the style's `sources` block.
//!
//! ## Source bindings
//!
//! Mirror the CLI's flow: bind every source declared in the style,
//! then render.
//!
//! ```js
//! r.bindSource("basemap", await fetchMvt(z, x, y));     // mvt / pmtiles bytes
//! r.bindSource("terrain", await fetchDem(z, x, y));     // raster-DEM bytes
//! // For DEM with neighbour-fetch on, bind the 8 surrounding tiles too:
//! for (const [dx, dy] of [[-1,-1],[0,-1],[1,-1],[-1,0],[1,0],
//!                          [-1,1],[0,1],[1,1]]) {
//!   r.bindSource("terrain", await fetchDem(z, x+dx, y+dy), { coord: [dx, dy] });
//! }
//! const png = r.renderTile(z, x, y);
//! r.clearSources();
//! ```
//!
//! The renderer dispatches on each source's declared `type`:
//! - `brush` → parse `.myb` JSON and register in the persistent
//!   brush bank under the source's `src` (no `clearSources` effect)
//! - `image` → decode PNG/WebP and register in the persistent image
//!   bank (same persistence)
//! - `sprite` → decode the atlas PNG (`bytes`) + resolve the index
//!   (inline in the style, or `opts.index` = the fetched sprite `.json`
//!   text) into the persistent sprite bank (same persistence)
//! - `mvt` / `pmtiles` → MVT decode + bind as `tile.<layer>` at render
//!   time (cleared by `clearSources`)
//! - `dem` → decode + 3×3 stitch + bind as `tile.<source-name>` at
//!   render time (cleared by `clearSources`)
//! - `geojson` → *remote* GeoJSON only: bind the fetched document
//!   `bytes`; projected per tile at render (cleared by `clearSources`).
//!   Inline `data` needs no bind — it's read from the style directly.
//! - `glyphs` → decode one SDF glyph-range PBF per call into the
//!   persistent glyph bank (repeat per range; unaffected by
//!   `clearSources`). This host cannot fetch ranges lazily, so bind
//!   every range the styled text will need *before* rendering — text
//!   whose range is missing drops those glyphs with a warning.
//!
//! ## Output
//!
//! `renderTile(z, x, y, opts?)` returns bytes in one of three formats
//! selected by `opts.format`: `"png"` (default), `"webp"` (lossless),
//! or `"rgba"` (straight un-premultiplied 8-bit RGBA, feed directly
//! into `ctx.putImageData`). `opts.tileSize` / `opts.pad` override the
//! style's canvas size for hi-DPI previews.
//!
//! ## Errors
//!
//! All fallible methods throw a JavaScript `Error` whose `.name`
//! discriminates the failure kind: `InvalidStyle`, `BrushParse`,
//! `MvtDecode`, `DemDecode`, `RasterDecode`, `GeoJsonDecode`,
//! `SpriteDecode`, `FontParse`, `GlyphDecode`, `RenderFailed`,
//! `PngEncode`, `WebpEncode`, `UnknownSource`.

mod log;

pub use log::LogSink;

use std::collections::HashMap;
use std::sync::Arc;

use ezu_features::FeatureLayer;
use ezu_graph::{
    build_graph, Cache, CanvasInfo, Evaluator, Graph, ParamValues, PortValue, SpriteSheet, TileId,
};
use ezu_paint::host::{
    build_sprite_icons, decode_dem_tile, decode_raster_tile, raster_to_png_with, raster_to_rgba8,
    raster_to_webp, stitch_padded_field, stitch_padded_raster, BrushBankLoader, DemTile,
    PngCompression, RasterTile, TileLoader,
};
use ezu_paint::nodes::default_registry;
use ezu_style::{Document, SourceDecl};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

const ERR_STYLE: &str = "InvalidStyle";
const ERR_BRUSH: &str = "BrushParse";
const ERR_MVT: &str = "MvtDecode";
const ERR_DEM: &str = "DemDecode";
const ERR_RASTER: &str = "RasterDecode";
const ERR_RENDER: &str = "RenderFailed";
const ERR_PNG: &str = "PngEncode";
const ERR_WEBP: &str = "WebpEncode";
const ERR_SOURCE: &str = "UnknownSource";
const ERR_GEOJSON: &str = "GeoJsonDecode";
const ERR_SPRITE: &str = "SpriteDecode";
const ERR_FONT: &str = "FontParse";
const ERR_GLYPHS: &str = "GlyphDecode";

/// Pending tile bytes for a single named source. MVT bytes are
/// validated at bind time (we attempt a decode and discard the
/// result) so errors surface immediately, but we keep the raw bytes
/// because `DecodedTile` isn't `Clone` and rendering wants to bind
/// freshly. DEM stays as raw bytes per `(dx, dy)` neighbour offset
/// until render time, when the centre tile id is known.
enum SourceBinding {
    /// Raw MVT bytes per `(dx, dy)` neighbour offset. The centre tile is
    /// `coord: [0, 0]` (default); neighbours (bound with `coord`) feed
    /// cross-tile label collision and are bound under `@dx,dy` names at
    /// render time. Re-decoded at render because `DecodedTile` isn't Clone.
    Mvt(HashMap<(i32, i32), Vec<u8>>),
    Dem(HashMap<(i32, i32), Vec<u8>>),
    /// RGBA imagery tiles per `(dx, dy)` neighbour offset, decoded +
    /// stitched at render time like DEM.
    Raster(HashMap<(i32, i32), Vec<u8>>),
    /// Raw GeoJSON bytes (WGS84 lon/lat) per `(dx, dy)` neighbour offset,
    /// projected into each tile frame at render time. Only needed for
    /// *remote* geojson; inline `data` is read straight from the document.
    GeoJson(HashMap<(i32, i32), Vec<u8>>),
}

/// Stateful WASM renderer.
#[wasm_bindgen]
pub struct Renderer {
    doc: Document,
    graph: Arc<Graph>,
    cache: Arc<Cache>,
    assets: BrushBankLoader,
    /// Pending source bindings, keyed by the `sources.<name>` entry in
    /// the style. Cleared by [`Renderer::clear_sources`].
    bindings: HashMap<String, SourceBinding>,
}

#[wasm_bindgen]
impl Renderer {
    /// Build a renderer from a style JSON document.
    #[wasm_bindgen(constructor)]
    pub fn new(style_json: &str) -> Result<Renderer, JsValue> {
        #[cfg(feature = "panic-hook")]
        console_error_panic_hook::set_once();

        let (doc, graph) = parse_and_build(style_json)?;
        Ok(Self {
            doc,
            graph: Arc::new(graph),
            cache: Arc::new(Cache::new()),
            assets: BrushBankLoader::new(),
            bindings: HashMap::new(),
        })
    }

    /// Replace the active style. Returns the new node count. Invalidates
    /// the intermediate cache and drops any pending source bindings.
    #[wasm_bindgen(js_name = setStyle)]
    pub fn set_style(&mut self, style_json: &str) -> Result<usize, JsValue> {
        let (doc, graph) = parse_and_build(style_json)?;
        let n = doc.nodes.len();
        self.doc = doc;
        self.graph = Arc::new(graph);
        self.cache = Arc::new(Cache::new());
        self.bindings.clear();
        Ok(n)
    }

    /// `tile-size` declared by the current style.
    #[wasm_bindgen(getter, js_name = tileSize)]
    pub fn tile_size(&self) -> u32 {
        self.doc.tile_size
    }

    /// Bind raw tile bytes under a `sources.<name>` entry from the style.
    /// The renderer dispatches on the source's declared `type`:
    ///
    /// - `mvt` / `pmtiles` → store raw MVT bytes per `(dx, dy)` neighbour
    ///   offset (centre `coord: [0, 0]`, the default). Layers are bound as
    ///   `<source>.<layer>` (centre) or `<source>.<layer>@dx,dy`
    ///   (neighbours) when [`render_tile`] runs. Binding only the centre
    ///   degrades cross-tile label collision to centre-only at borders.
    /// - `dem` / `raster` → store the raw bytes per `(dx, dy)` neighbour
    ///   offset. The centre tile is `coord: [0, 0]` (default). Decoding
    ///   and 3×3 stitching happen at render time once the tile id is known.
    /// - `geojson` (remote) → store raw bytes per `(dx, dy)` offset;
    ///   projected per tile (and neighbour) at render time.
    ///
    /// `opts` is a JS object: `{ coord?: [dx, dy] }`. Throws
    /// `UnknownSource` if `name` doesn't match any entry in the style's
    /// `sources` block, `MvtDecode` if MVT bytes don't parse, and
    /// `DemDecode` for non-image DEM bytes (the decode itself runs at
    /// render time, but obvious cases are caught here).
    #[wasm_bindgen(js_name = bindSource)]
    pub fn bind_source(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
        opts: Option<js_sys::Object>,
    ) -> Result<(), JsValue> {
        let decl = self
            .doc
            .sources
            .get(name)
            .ok_or_else(|| named_err(ERR_SOURCE, format!("no source `{name}` in style")))?;
        match decl {
            SourceDecl::Brush(file) => {
                // Brushes are document-scoped: register into the
                // persistent BrushBankLoader keyed by `decl.src`
                // (which is what the `brush-file` node looks up).
                let src_key = file.src.clone();
                let bytes_str = std::str::from_utf8(&bytes).map_err(|e| named_err(ERR_BRUSH, e))?;
                let brush =
                    hokusai::myb::from_str(bytes_str).map_err(|e| named_err(ERR_BRUSH, e))?;
                self.assets.insert(src_key, brush);
            }
            SourceDecl::Image(file) => {
                // Same persistent lifetime as brushes — keyed by
                // `decl.src` so the `image` node finds it.
                let src_key = file.src.clone();
                let raster = ezu_paint::host::decode_image_bytes(&bytes)
                    .map_err(|e| named_err(ERR_MVT, format!("image decode: {e}")))?;
                self.assets.insert_image(src_key, raster);
            }
            SourceDecl::Mvt(_) | SourceDecl::Pmtiles(_) => {
                // Validate now so a malformed payload throws at bind
                // time rather than render time — we toss the result and
                // re-decode at render since `DecodedTile` isn't Clone.
                let _ = ezu_features::mvt::decode(&bytes).map_err(|e| named_err(ERR_MVT, e))?;
                let coord = parse_coord_opt(opts.as_ref())?;
                let entry = self
                    .bindings
                    .entry(name.to_string())
                    .or_insert_with(|| SourceBinding::Mvt(HashMap::new()));
                if let SourceBinding::Mvt(map) = entry {
                    map.insert(coord, bytes);
                } else {
                    return Err(named_err(
                        ERR_SOURCE,
                        format!("source `{name}` already bound as a different kind"),
                    ));
                }
            }
            SourceDecl::Dem(_) => {
                let coord = parse_coord_opt(opts.as_ref())?;
                let entry = self
                    .bindings
                    .entry(name.to_string())
                    .or_insert_with(|| SourceBinding::Dem(HashMap::new()));
                if let SourceBinding::Dem(map) = entry {
                    map.insert(coord, bytes);
                } else {
                    return Err(named_err(
                        ERR_SOURCE,
                        format!("source `{name}` already bound as a different kind"),
                    ));
                }
            }
            SourceDecl::Raster(_) => {
                let coord = parse_coord_opt(opts.as_ref())?;
                let entry = self
                    .bindings
                    .entry(name.to_string())
                    .or_insert_with(|| SourceBinding::Raster(HashMap::new()));
                if let SourceBinding::Raster(map) = entry {
                    map.insert(coord, bytes);
                } else {
                    return Err(named_err(
                        ERR_SOURCE,
                        format!("source `{name}` already bound as a different kind"),
                    ));
                }
            }
            // Remote GeoJSON: the JS host fetched the document and hands the
            // raw bytes here. (Inline `data` needs no bind — it's read from
            // the document at render time.) Stored tile-scoped; projected per
            // tile when `render_tile` runs.
            SourceDecl::GeoJson(_) => {
                // Validate now so malformed bytes throw at bind time.
                serde_json::from_slice::<serde_json::Value>(&bytes)
                    .map_err(|e| named_err(ERR_GEOJSON, e))?;
                let coord = parse_coord_opt(opts.as_ref())?;
                let entry = self
                    .bindings
                    .entry(name.to_string())
                    .or_insert_with(|| SourceBinding::GeoJson(HashMap::new()));
                if let SourceBinding::GeoJson(map) = entry {
                    map.insert(coord, bytes);
                } else {
                    return Err(named_err(
                        ERR_SOURCE,
                        format!("source `{name}` already bound as a different kind"),
                    ));
                }
            }
            // Sprite: the JS host provides the atlas PNG as `bytes`. The index
            // is either inline in the document or supplied as `opts.index` (the
            // fetched sprite `.json` text). Built once into the persistent
            // bank, like brushes/images (unaffected by `clearSources`).
            SourceDecl::Sprite(sprite) => {
                let atlas = ezu_paint::host::decode_image_bytes(&bytes)
                    .map_err(|e| named_err(ERR_SPRITE, format!("atlas decode: {e}")))?;
                let index_json = parse_index_opt(opts.as_ref());
                let icons = build_sprite_icons(&sprite.index, index_json.as_deref())
                    .map_err(|e| named_err(ERR_SPRITE, e))?;
                self.assets
                    .insert_sprite(sprite.image.clone(), SpriteSheet { atlas, icons });
            }
            // Font: the JS host provides the raw TTF/OTF/TTC bytes. Built
            // once into the persistent bank keyed by the source's `url`,
            // like brushes/images (unaffected by `clearSources`).
            SourceDecl::Font(font) => {
                let face = ezu_core::text::Font::from_bytes(bytes.into(), font.index)
                    .map_err(|e| named_err(ERR_FONT, e))?;
                self.assets.insert_font(font.url.clone(), face);
            }
            // Glyphs: the JS host provides one raw range PBF per call
            // (which range is read from the message itself); repeated
            // calls accumulate ranges into one persistent stack. This
            // host cannot fetch lazily, so *every* range the styled
            // text will need must be bound before rendering — a label
            // whose range is missing drops its glyphs with a warning.
            SourceDecl::Glyphs(glyphs) => {
                let key = glyphs.asset_key();
                let stack = {
                    let bank = self
                        .assets
                        .glyphs
                        .read()
                        .expect("glyphs bank poisoned")
                        .get(&key)
                        .cloned();
                    match bank {
                        Some(stack) => stack,
                        None => {
                            let stack = Arc::new(ezu_core::text::SdfFontStack::new());
                            self.assets.insert_glyphs(key, stack.clone());
                            stack
                        }
                    }
                };
                stack
                    .insert_range(&bytes)
                    .map_err(|e| named_err(ERR_GLYPHS, e))?;
            }
        }
        Ok(())
    }

    /// Effective attribution declared by the style (document +
    /// sources), joined with ` | `. Upstream TileJSON / PMTiles
    /// metadata is the JS host's concern — merge it on that side.
    #[wasm_bindgen(getter)]
    pub fn attribution(&self) -> Option<String> {
        let list = self.doc.attributions();
        if list.is_empty() {
            None
        } else {
            Some(list.join(" | "))
        }
    }

    /// Drop every pending source binding. Call between tile renders.
    #[wasm_bindgen(js_name = clearSources)]
    pub fn clear_sources(&mut self) {
        self.bindings.clear();
    }

    /// Names of every source with at least one pending binding.
    /// Order matches the style's `sources` declaration order.
    #[wasm_bindgen(js_name = boundSources)]
    pub fn bound_sources(&self) -> Vec<String> {
        self.doc
            .sources
            .keys()
            .filter(|n| self.bindings.contains_key(n.as_str()))
            .cloned()
            .collect()
    }

    /// Render a single tile using whatever sources are currently bound.
    ///
    /// `opts` (JS object, all fields optional):
    /// - `format`: `"png"` (default) / `"webp"` / `"rgba"`
    /// - `tileSize`, `pad`: override the style's canvas size for this
    ///   call (hi-DPI / preview)
    /// - `png`: `{ compression?: "fast" | "default" | "best" }`
    #[wasm_bindgen(js_name = renderTile)]
    pub fn render_tile(
        &self,
        z: u8,
        x: u32,
        y: u32,
        opts: Option<js_sys::Object>,
    ) -> Result<Vec<u8>, JsValue> {
        let parsed = parse_render_options(opts.as_ref());
        self.render_with_bindings(z, x, y, parsed)
    }
}

#[derive(Clone, Copy)]
enum OutputFormat {
    Png,
    Webp,
    Rgba,
}

impl Renderer {
    /// Render path used by the new `renderTile` API. Reads pending
    /// source bindings from `self.bindings` and dispatches each based
    /// on its declared kind in the style.
    fn render_with_bindings(
        &self,
        z: u8,
        x: u32,
        y: u32,
        opts: RenderOptions,
    ) -> Result<Vec<u8>, JsValue> {
        let tile_size = opts.tile_size.unwrap_or(self.doc.tile_size);
        let pad = opts.pad.unwrap_or(self.doc.pad);
        let tile_id = TileId { z, x, y };
        let canvas = CanvasInfo { tile_size, pad };
        let mut tile_loader = TileLoader::new(&self.assets, tile_id);

        for (name, binding) in &self.bindings {
            match binding {
                SourceBinding::Mvt(byte_map) => {
                    // Centre under `<source>.<layer>`, any neighbours the
                    // host bound under `@dx,dy` (cross-tile collision). A
                    // host binding only the centre degrades to centre-only
                    // collision at borders — no error.
                    for (&(dx, dy), bytes) in byte_map {
                        let decoded =
                            ezu_features::mvt::decode(bytes).map_err(|e| named_err(ERR_MVT, e))?;
                        tile_loader.bind_mvt_neighbor(name, dx, dy, decoded);
                    }
                }
                SourceBinding::Dem(byte_map) => {
                    let encoding = match self.doc.sources.get(name) {
                        Some(SourceDecl::Dem(spec)) => spec.encoding,
                        _ => {
                            return Err(named_err(
                                ERR_SOURCE,
                                format!("source `{name}` is no longer a DEM in the active style"),
                            ));
                        }
                    };
                    let elevation_offset = match self.doc.sources.get(name) {
                        Some(SourceDecl::Dem(spec)) => spec.elevation_offset,
                        _ => 0.0,
                    };
                    let mut decoded: HashMap<(i32, i32), DemTile> =
                        HashMap::with_capacity(byte_map.len());
                    for (&(dx, dy), bytes) in byte_map {
                        // Resolve absolute tile coords so decode errors
                        // identify the source tile, not just the offset.
                        let abs_x = (x as i32 + dx) as u32;
                        let abs_y = (y as i32 + dy) as u32;
                        let t = decode_dem_tile(bytes, encoding, z, abs_x, abs_y)
                            .map_err(|e| named_err(ERR_DEM, e))?;
                        decoded.insert((dx, dy), t);
                    }
                    let borrowed: HashMap<(i32, i32), &DemTile> =
                        decoded.iter().map(|(k, v)| (*k, v)).collect();
                    let field = stitch_padded_field(&borrowed, elevation_offset, tile_id, canvas)
                        .ok_or_else(|| {
                        named_err(
                            ERR_DEM,
                            format!(
                                "source `{name}`: missing centre tile (bind with coord [0, 0])"
                            ),
                        )
                    })?;
                    // Bare source name — the `dem` node's asset lookup key.
                    tile_loader.bind_scalar_field(name.clone(), field);
                }
                SourceBinding::Raster(byte_map) => {
                    let mut decoded: HashMap<(i32, i32), RasterTile> =
                        HashMap::with_capacity(byte_map.len());
                    for (&(dx, dy), bytes) in byte_map {
                        let abs_x = (x as i32 + dx) as u32;
                        let abs_y = (y as i32 + dy) as u32;
                        let t = decode_raster_tile(bytes, z, abs_x, abs_y)
                            .map_err(|e| named_err(ERR_RASTER, e))?;
                        decoded.insert((dx, dy), t);
                    }
                    let borrowed: HashMap<(i32, i32), &RasterTile> =
                        decoded.iter().map(|(k, v)| (*k, v)).collect();
                    let buf = stitch_padded_raster(&borrowed, canvas).ok_or_else(|| {
                        named_err(
                            ERR_RASTER,
                            format!(
                                "source `{name}`: missing centre tile (bind with coord [0, 0])"
                            ),
                        )
                    })?;
                    tile_loader.bind_raster(name.clone(), buf);
                }
                SourceBinding::GeoJson(byte_map) => {
                    for (&(dx, dy), bytes) in byte_map {
                        let data: serde_json::Value =
                            serde_json::from_slice(bytes).map_err(|e| named_err(ERR_GEOJSON, e))?;
                        bind_geojson(&mut tile_loader, name, &data, tile_id, dx, dy)?;
                    }
                }
            }
        }

        // Inline GeoJSON needs no `bindSource` — project the document's `data`
        // straight into this tile. Skip any that were bound remotely above.
        // When the graph asks for neighbour features (cross-tile collision),
        // project into those neighbour tiles too and bind under `@dx,dy`.
        let requested = self.graph.asset_inputs();
        for (name, decl) in &self.doc.sources {
            if let SourceDecl::GeoJson(g) = decl {
                if self.bindings.contains_key(name) {
                    continue;
                }
                if let Some(data) = g.data.as_ref().filter(|d| d.is_object() || d.is_array()) {
                    bind_geojson(&mut tile_loader, name, data, tile_id, 0, 0)?;
                    for (dx, dy) in ezu_paint::host::requested_neighbor_offsets(&requested, name) {
                        bind_geojson(&mut tile_loader, name, data, tile_id, dx, dy)?;
                    }
                }
            }
        }

        encode_render(
            &self.graph,
            &self.cache,
            &tile_loader,
            tile_id,
            canvas,
            opts,
        )
    }
}

/// Shared finish step: evaluate the graph and encode the result.
fn encode_render(
    graph: &Graph,
    cache: &Cache,
    tile_loader: &TileLoader<'_>,
    tile_id: TileId,
    canvas: CanvasInfo,
    opts: RenderOptions,
) -> Result<Vec<u8>, JsValue> {
    let ev = Evaluator::new(graph, cache, tile_loader);
    let params = ParamValues::new();
    let seed = tile_seed(tile_id.z, tile_id.x, tile_id.y);
    // The parallel evaluator is used only when the caller opts in with
    // `{ parallel: true }`, which the host does exactly when it has
    // initialized a thread pool (`initThreadPool` resolved). Before that
    // — and in every single-threaded build, where `render_parallel`
    // transparently falls back to `render` — we stay sequential:
    // `render_parallel` would otherwise touch rayon's global pool, which
    // can't be built on wasm without workers. Both paths are
    // deterministic and produce identical output.
    let out = if opts.parallel {
        ev.render_parallel(tile_id, canvas, &params, seed)
    } else {
        ev.render(tile_id, canvas, &params, seed)
    }
    .map_err(|e| named_err(ERR_RENDER, e))?;
    let raster = match out {
        PortValue::Raster(r) => r,
        other => {
            return Err(named_err(
                ERR_RENDER,
                format!("expected Raster output, got {:?}", other.kind()),
            ))
        }
    };
    Ok(match opts.format {
        OutputFormat::Png => {
            raster_to_png_with(&raster, canvas.tile_size, canvas.pad, opts.png_compression)
                .map_err(|e| named_err(ERR_PNG, e))?
        }
        OutputFormat::Webp => raster_to_webp(&raster, canvas.tile_size, canvas.pad)
            .map_err(|e| named_err(ERR_WEBP, e))?,
        OutputFormat::Rgba => raster_to_rgba8(&raster, canvas.tile_size, canvas.pad),
    })
}

/// Parsed `renderTile` options (format + canvas + png compression).
#[derive(Clone, Copy)]
struct RenderOptions {
    format: OutputFormat,
    tile_size: Option<u32>,
    pad: Option<u32>,
    png_compression: PngCompression,
    /// Opt into the parallel evaluator. Only meaningful in `threads`
    /// builds once `initThreadPool` has resolved; otherwise ignored
    /// (`render_parallel` falls back to sequential evaluation).
    parallel: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            format: OutputFormat::Png,
            tile_size: None,
            pad: None,
            png_compression: PngCompression::Default,
            parallel: false,
        }
    }
}

/// Parse the `renderTile` options object. Unknown keys are silently
/// ignored so future fields stay backwards-compatible.
fn parse_render_options(obj: Option<&js_sys::Object>) -> RenderOptions {
    let mut out = RenderOptions::default();
    let Some(obj) = obj else {
        return out;
    };
    // format: "png" | "webp" | "rgba"
    if let Some(s) = js_sys::Reflect::get(obj, &"format".into())
        .ok()
        .and_then(|v| v.as_string())
    {
        out.format = match s.as_str() {
            "webp" => OutputFormat::Webp,
            "rgba" => OutputFormat::Rgba,
            _ => OutputFormat::Png,
        };
    }
    if let Some(n) = js_sys::Reflect::get(obj, &"tileSize".into())
        .ok()
        .and_then(|v| v.as_f64())
    {
        out.tile_size = Some(n as u32);
    }
    if let Some(n) = js_sys::Reflect::get(obj, &"pad".into())
        .ok()
        .and_then(|v| v.as_f64())
    {
        out.pad = Some(n as u32);
    }
    if let Some(b) = js_sys::Reflect::get(obj, &"parallel".into())
        .ok()
        .and_then(|v| v.as_bool())
    {
        out.parallel = b;
    }
    let png = js_sys::Reflect::get(obj, &"png".into()).unwrap_or(JsValue::UNDEFINED);
    if let Some(png_obj) = png.dyn_ref::<js_sys::Object>() {
        if let Some(s) = js_sys::Reflect::get(png_obj, &"compression".into())
            .ok()
            .and_then(|v| v.as_string())
        {
            out.png_compression = match s.as_str() {
                "fast" => PngCompression::Fast,
                "best" => PngCompression::Best,
                _ => PngCompression::Default,
            };
        }
    }
    out
}

/// Project WGS84 GeoJSON `data` into `tile`'s local frame (extent 4096) and
/// bind it as one feature layer under `<name>.<name>` — matching a
/// converter-emitted `features` node's `(source, source)` target.
/// Project inline/remote GeoJSON into the tile at neighbour offset
/// `(dx, dy)` (`(0, 0)` = the tile itself) and bind it under
/// `<name>.<name>` (own) or `<name>.<name>@dx,dy` (neighbour). Neighbour
/// `x` wraps at the antimeridian; out-of-range `y` (poles) is skipped.
fn bind_geojson(
    tile_loader: &mut TileLoader<'_>,
    name: &str,
    data: &serde_json::Value,
    tile: TileId,
    dx: i32,
    dy: i32,
) -> Result<(), JsValue> {
    let world = 1i64 << tile.z;
    let ny = tile.y as i64 + dy as i64;
    if ny < 0 || ny >= world {
        return Ok(()); // top/bottom edge: no neighbour in Y
    }
    let nx = (tile.x as i64 + dx as i64).rem_euclid(world) as u32;
    let features = ezu_features::geojson::decode_projected(data, tile.z, nx, ny as u32, 4096)
        .map_err(|e| named_err(ERR_GEOJSON, e))?;
    let base = format!("{name}.{name}");
    tile_loader.bind_features(
        ezu_graph::neighbor_binding(&base, dx, dy),
        FeatureLayer {
            name: name.to_string(),
            extent: 4096,
            features,
        },
    );
    Ok(())
}

/// Parse the optional `{ index: "<sprite-json text>" }` payload to
/// `bindSource` for a sprite (the fetched index when it's a URL, not inline).
fn parse_index_opt(obj: Option<&js_sys::Object>) -> Option<String> {
    js_sys::Reflect::get(obj?, &"index".into())
        .ok()
        .and_then(|v| v.as_string())
}

/// Parse the optional `{ coord: [dx, dy] }` payload to `bindSource`.
fn parse_coord_opt(obj: Option<&js_sys::Object>) -> Result<(i32, i32), JsValue> {
    let Some(obj) = obj else {
        return Ok((0, 0));
    };
    let coord = js_sys::Reflect::get(obj, &"coord".into()).unwrap_or(JsValue::UNDEFINED);
    if coord.is_undefined() || coord.is_null() {
        return Ok((0, 0));
    }
    let arr = coord
        .dyn_into::<js_sys::Array>()
        .map_err(|_| named_err(ERR_SOURCE, "coord must be a [dx, dy] array of two numbers"))?;
    if arr.length() != 2 {
        return Err(named_err(
            ERR_SOURCE,
            "coord must have exactly two numbers [dx, dy]",
        ));
    }
    let dx =
        arr.get(0)
            .as_f64()
            .ok_or_else(|| named_err(ERR_SOURCE, "coord[0] (dx) must be a number"))? as i32;
    let dy =
        arr.get(1)
            .as_f64()
            .ok_or_else(|| named_err(ERR_SOURCE, "coord[1] (dy) must be a number"))? as i32;
    Ok((dx, dy))
}

fn parse_and_build(style_json: &str) -> Result<(Document, Graph), JsValue> {
    let doc = Document::from_json(style_json).map_err(|e| named_err(ERR_STYLE, e))?;
    let registry = default_registry();
    let graph = build_graph(&doc, &registry).map_err(|e| named_err(ERR_STYLE, e))?;
    Ok((doc, graph))
}

fn tile_seed(z: u8, x: u32, y: u32) -> u64 {
    let mut s = 0u64;
    s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(z as u64);
    s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(x as u64);
    s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(y as u64);
    s
}

/// Whether the wasm binary was compiled with `+simd128`. Lets the demo
/// page label the renderer build accurately.
#[wasm_bindgen(js_name = simdEnabled)]
pub fn simd_enabled() -> bool {
    cfg!(target_feature = "simd128")
}

/// Whether this build supports multithreaded rendering (compiled with
/// the `threads` feature). When `false`, `initThreadPool` is absent and
/// `renderTile`'s `parallel` option is a no-op. Mirrors `simdEnabled` so
/// the host can label the build.
#[wasm_bindgen(js_name = threadsEnabled)]
pub fn threads_enabled() -> bool {
    cfg!(feature = "threads")
}

/// Initialize the rayon thread pool backing multithreaded rendering.
///
/// `initThreadPool(num_threads)` — usually `navigator.hardwareConcurrency`
/// — is re-exported from `wasm-bindgen-rayon`. Call it once after
/// `init()`, `await` the returned promise, then pass `{ parallel: true }`
/// to `renderTile`. It requires a cross-origin-isolated page (COOP:
/// same-origin + COEP: require-corp) so `SharedArrayBuffer` is available.
///
/// ```js
/// import init, { initThreadPool, threadsEnabled, Renderer } from "./ezu_wasm.js";
/// await init();
/// let parallel = false;
/// if (threadsEnabled() && self.crossOriginIsolated) {
///   await initThreadPool(navigator.hardwareConcurrency);
///   parallel = true;
/// }
/// const r = new Renderer(styleJson);
/// r.renderTile(z, x, y, { format: "rgba", parallel });
/// ```
#[cfg(feature = "threads")]
pub use wasm_bindgen_rayon::init_thread_pool;

/// Build a JS `Error` whose `.name` discriminates the failure kind so callers
/// can dispatch on it.
fn named_err(name: &str, e: impl std::fmt::Display) -> JsValue {
    let err = js_sys::Error::new(&e.to_string());
    err.set_name(name);
    err.into()
}
