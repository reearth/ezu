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
//! - `glyphs` → decode one SDF glyph PBF per call into the persistent
//!   glyph bank (a whole `{range}.pbf`, or a subset spanning several
//!   ranges — glyphs are filed by id; repeat to accumulate; unaffected
//!   by `clearSources`). This host cannot fetch glyphs lazily, so bind
//!   everything the styled text will need *before* rendering — text
//!   whose glyphs are missing drops them with a warning.
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
//!
//! `OutOfMemory` is thrown from wherever the heap ran out — see
//! [`oom`] for what the host may and may not do afterwards.

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
/// Only the wasm build installs the allocator that throws it.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const ERR_OOM: &str = "OutOfMemory";

/// Turn heap exhaustion into a JavaScript exception.
///
/// Rust's reaction to a failed allocation is `handle_alloc_error`, which
/// on wasm is an `unreachable` trap. The instance dies mid-call, the
/// return value is never written, and the JS glue reads whatever happens
/// to sit at the return pointer — which is how an out-of-memory render
/// surfaces as a bewildering `RangeError: Invalid array buffer length`
/// from `getArrayU8FromWasm0`, with nothing naming the real cause.
///
/// This allocator intercepts the null the underlying allocator returns
/// when `memory.grow` is refused (a 128 MB Workers isolate refusing to
/// grow, say) and throws a JS `Error` with `.name === "OutOfMemory"`
/// from that exact point, so the host can branch on it:
///
/// ```js
/// try { png = renderer.renderTile(z, x, y); }
/// catch (e) { if (e.name === "OutOfMemory") { /* smaller tile, or bail */ } }
/// ```
///
/// Limits, which callers must respect:
///
/// - **The renderer instance is finished.** The exception unwinds the
///   wasm frames without running any Rust cleanup: locks stay locked,
///   half-built values leak, and the allocator's own bookkeeping is
///   whatever it was mid-call. Drop the module instance and, if the host
///   retries, build a fresh one. Nothing here makes OOM recoverable *in
///   place* — it makes it diagnosable.
/// - It only fires for allocation failure. A wasm stack overflow, or a
///   host that kills the isolate for exceeding a memory cap rather than
///   refusing `memory.grow`, still ends the instance without warning.
#[cfg(target_arch = "wasm32")]
mod oom {
    use std::alloc::{GlobalAlloc, Layout, System};

    pub struct ThrowOnOom;

    /// Throw a typed JS error and never return. Takes no allocation on
    /// the Rust side: `js_sys::Error::new` and `set_name` pass the
    /// message by pointer and length out of already-live memory, which
    /// is what makes this safe to call from inside the allocator.
    #[cold]
    #[inline(never)]
    fn throw_oom(size: usize) -> ! {
        let err = js_sys::Error::new("out of memory: the wasm heap could not grow");
        err.set_name(super::ERR_OOM);
        let _ = js_sys::Reflect::set(
            &err,
            &wasm_bindgen::JsValue::from_str("requestedBytes"),
            &wasm_bindgen::JsValue::from_f64(size as f64),
        );
        wasm_bindgen::throw_val(err.into())
    }

    unsafe impl GlobalAlloc for ThrowOnOom {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let p = System.alloc(layout);
            if p.is_null() {
                throw_oom(layout.size());
            }
            p
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let p = System.alloc_zeroed(layout);
            if p.is_null() {
                throw_oom(layout.size());
            }
            p
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let p = System.realloc(ptr, layout, new_size);
            if p.is_null() {
                throw_oom(new_size);
            }
            p
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            System.dealloc(ptr, layout)
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[global_allocator]
static ALLOC: oom::ThrowOnOom = oom::ThrowOnOom;

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
            // Glyphs: the JS host provides one raw glyph PBF per call
            // and repeated calls accumulate into one persistent stack.
            // Each glyph is filed under its own id, so a message is
            // free to be a subset spanning several ranges (see
            // `neededCodepoints`) as well as a whole `{range}.pbf`.
            // This host cannot fetch lazily, so *every* glyph the
            // styled text will need must be bound before rendering — a
            // label whose glyphs are missing drops them with a warning.
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

    /// Neighbour tile offsets the active style actually asks for from
    /// `source`, as an array of `[dx, dy]` pairs (never including the
    /// centre `[0, 0]`).
    ///
    /// Cross-tile label collision and edge-continuous DEM shading are the
    /// only things that read neighbours, and only for the sources they
    /// name. A host that fetches the full 3×3 window unconditionally
    /// therefore pays for up to eight tiles it will not look at; passing
    /// each offset from this list to `bindSource(name, bytes, { coord })`
    /// fetches exactly what the recipe needs. An empty array means the
    /// centre tile is enough.
    ///
    /// Throws `UnknownSource` if `name` is not declared in the style.
    #[wasm_bindgen(js_name = requestedNeighborOffsets)]
    pub fn requested_neighbor_offsets(&self, name: &str) -> Result<js_sys::Array, JsValue> {
        if !self.doc.sources.contains_key(name) {
            return Err(named_err(
                ERR_SOURCE,
                format!("no source `{name}` in style"),
            ));
        }
        let requested = self.graph.asset_inputs();
        let out = js_sys::Array::new();
        for (dx, dy) in ezu_paint::host::requested_neighbor_offsets(&requested, name) {
            let pair = js_sys::Array::new();
            pair.push(&JsValue::from(dx));
            pair.push(&JsValue::from(dy));
            out.push(&pair);
        }
        Ok(out)
    }

    /// Codepoints the currently bound features can require, as
    /// `{ [glyphsSourceName]: number[] }` sorted ascending.
    ///
    /// This is the precise form of
    /// [`neededGlyphRanges`](Self::needed_glyph_ranges): a host that
    /// can build its own glyph PBF — one message holding just these
    /// codepoints — transfers only the glyphs the tile draws instead
    /// of the whole 256-codepoint block around each of them. On CJK
    /// labels that is the difference between a few thousand glyphs and
    /// a few tens of megabytes. `bindSource` files each glyph by its
    /// own id, so such a subset may span any number of blocks and
    /// needs no particular `range` string.
    ///
    /// Hosts that can only fetch whole `{range}.pbf` files off a
    /// MapLibre glyphs endpoint want `neededGlyphRanges` instead. Both
    /// calls see the same set of codepoints and carry the same
    /// over-approximation caveat.
    #[wasm_bindgen(js_name = neededCodepoints)]
    pub fn needed_codepoints(&self) -> Result<js_sys::Object, JsValue> {
        self.needed_glyphs_object(|cp| cp)
    }

    /// Glyph ranges the currently bound features can require, as
    /// `{ [glyphsSourceName]: number[] }` where each number is a range
    /// start (`0`, `256`, `512`, …) — i.e. the `{range}` in a
    /// `…/{fontstack}/{range}.pbf` URL is `<start>-<start + 255>`.
    ///
    /// This host cannot fetch glyph ranges lazily, so every range a
    /// tile's labels touch must be bound before `renderTile`. Rather than
    /// scraping every string in the MVT, call this after binding the
    /// vector sources and bind exactly the listed ranges.
    ///
    /// A range holds 256 codepoints and a tile typically draws a
    /// handful of them, so this is a coarse unit to fetch in. Hosts
    /// that can assemble their own subset PBF should call
    /// [`neededCodepoints`](Self::needed_codepoints) instead.
    ///
    /// It is an over-approximation, deliberately: a range is listed if
    /// *any* feature in a text layer carries the codepoint in a property
    /// the layer's `text` expression reads, without evaluating filters,
    /// zoom ranges, or the expression itself, and it is listed for every
    /// fontstack in that layer's fallback chain. So it never omits a
    /// range a label needs, and it may name a few that go unused. Text
    /// layers that build their string from something other than a `get`
    /// of a feature property (a literal, a `concat` of formatted values)
    /// contribute their literal text where it is a plain string.
    #[wasm_bindgen(js_name = neededGlyphRanges)]
    pub fn needed_glyph_ranges(&self) -> Result<js_sys::Object, JsValue> {
        self.needed_glyphs_object(|cp| cp & !0xFF)
    }

    /// Shared body of the two prepass calls: the needed codepoints per
    /// glyphs source, mapped through `unit` (identity, or the range
    /// start containing it) and deduped.
    fn needed_glyphs_object(&self, unit: fn(u32) -> u32) -> Result<js_sys::Object, JsValue> {
        let out = js_sys::Object::new();
        for (source, codepoints) in self.needed_codepoints_by_source()? {
            let mut units: Vec<u32> = codepoints.into_iter().map(unit).collect();
            units.dedup();
            let arr = js_sys::Array::new();
            for u in units {
                arr.push(&JsValue::from(u));
            }
            js_sys::Reflect::set(&out, &JsValue::from_str(source), &arr)?;
        }
        Ok(out)
    }

    /// Glyphs source name → the BMP codepoints its fontstacks can be
    /// asked to shape, over-approximated as documented on
    /// [`neededGlyphRanges`](Self::needed_glyph_ranges).
    fn needed_codepoints_by_source(
        &self,
    ) -> Result<std::collections::BTreeMap<&str, std::collections::BTreeSet<u32>>, JsValue> {
        let mut needed: std::collections::BTreeMap<&str, std::collections::BTreeSet<u32>> =
            std::collections::BTreeMap::new();

        let decoded = self.decode_bound_features()?;
        for spec in self.doc.nodes.values() {
            let Some(stacks) = glyph_source_names(&self.doc, &spec.fields) else {
                continue;
            };
            let Some(text) = spec.fields.get("text") else {
                continue;
            };
            let mut codepoints: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
            // A literal `text` needs no features at all.
            if let Some(lit) = text.as_str() {
                if !lit.starts_with('@') && !lit.starts_with('$') {
                    collect_codepoints(lit, &mut codepoints);
                }
            }
            let props = referenced_properties(text);
            if !props.is_empty() {
                let source = spec.fields.get("source").and_then(|v| v.as_str());
                let layer = spec.fields.get("layer").and_then(|v| v.as_str());
                for (bound_source, tile) in &decoded {
                    if source.is_some_and(|s| s != *bound_source) {
                        continue;
                    }
                    for l in &tile.layers {
                        if layer.is_some_and(|want| want != l.name) {
                            continue;
                        }
                        for f in &l.features {
                            for p in &props {
                                if let Some(ezu_features::Value::String(s)) = f.properties.get(*p) {
                                    collect_codepoints(s, &mut codepoints);
                                }
                            }
                        }
                    }
                }
            }
            for stack in stacks {
                needed.entry(stack).or_default().extend(codepoints.iter());
            }
        }
        Ok(needed)
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
    /// Decode every bound MVT source once, centre and neighbours alike,
    /// for inspection ahead of a render.
    fn decode_bound_features(
        &self,
    ) -> Result<Vec<(&str, ezu_features::mvt::DecodedTile)>, JsValue> {
        let mut out = Vec::new();
        for (name, binding) in &self.bindings {
            let SourceBinding::Mvt(byte_map) = binding else {
                continue;
            };
            for bytes in byte_map.values() {
                let tile = ezu_features::mvt::decode(bytes).map_err(|e| named_err(ERR_MVT, e))?;
                out.push((name.as_str(), tile));
            }
        }
        Ok(out)
    }

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

/// The `glyphs` source names a node's `font` stack resolves to, or
/// `None` when the node has no font stack (so it is not a text node).
///
/// A `font` entry names a source; only the ones declared as `glyphs`
/// have ranges to bind, and their *source* name is what `bindSource`
/// takes.
fn glyph_source_names<'a>(
    doc: &'a Document,
    fields: &serde_json::Map<String, serde_json::Value>,
) -> Option<Vec<&'a str>> {
    let font = fields.get("font")?;
    let names: Vec<&str> = match font {
        serde_json::Value::String(s) => vec![s.as_str()],
        serde_json::Value::Array(a) => a.iter().filter_map(|v| v.as_str()).collect(),
        _ => return None,
    };
    Some(
        names
            .into_iter()
            .filter_map(|n| {
                let n = n.strip_prefix('@').unwrap_or(n);
                doc.sources
                    .get_key_value(n)
                    .filter(|(_, decl)| matches!(decl, SourceDecl::Glyphs(_)))
                    .map(|(k, _)| k.as_str())
            })
            .collect(),
    )
}

/// Feature property names a `text` expression reads, i.e. every
/// `["get", "<name>"]` anywhere inside it.
fn referenced_properties(expr: &serde_json::Value) -> Vec<&str> {
    fn walk<'a>(v: &'a serde_json::Value, out: &mut Vec<&'a str>) {
        match v {
            serde_json::Value::Array(items) => {
                if let (Some("get"), Some(serde_json::Value::String(name))) = (
                    items.first().and_then(|h| h.as_str()),
                    items.get(1).filter(|_| items.len() == 2),
                ) {
                    out.push(name.as_str());
                }
                for item in items {
                    walk(item, out);
                }
            }
            serde_json::Value::Object(map) => {
                for item in map.values() {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(expr, &mut out);
    out.sort_unstable();
    out.dedup();
    out
}

/// Add the codepoint of every character in `s`. Codepoints outside
/// the Basic Multilingual Plane are skipped: the glyph protocol cannot
/// address them, so nothing can serve them.
fn collect_codepoints(s: &str, out: &mut std::collections::BTreeSet<u32>) {
    for c in s.chars() {
        let cp = c as u32;
        if cp <= 0xFFFF {
            out.insert(cp);
        }
    }
}

/// Build a JS `Error` whose `.name` discriminates the failure kind so callers
/// can dispatch on it.
fn named_err(name: &str, e: impl std::fmt::Display) -> JsValue {
    let err = js_sys::Error::new(&e.to_string());
    err.set_name(name);
    err.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_expressions_yield_the_properties_they_read() {
        let expr = serde_json::json!([
            "concat",
            ["get", "name:en"],
            " (",
            ["coalesce", ["get", "ref"], ["get", "name"]],
            ")"
        ]);
        assert_eq!(
            referenced_properties(&expr),
            vec!["name", "name:en", "ref"],
            "every `get` in the expression, deduplicated"
        );
        // A `get` with extra arguments reads from a supplied object, not
        // the feature, so it names no feature property.
        assert!(referenced_properties(&serde_json::json!(["get", "a", ["x"]])).is_empty());
        assert!(referenced_properties(&serde_json::json!("plain")).is_empty());
    }

    #[test]
    fn codepoints_are_collected_sorted_and_deduplicated() {
        let mut out = std::collections::BTreeSet::new();
        collect_codepoints("AZA", &mut out);
        collect_codepoints("東京", &mut out);
        assert_eq!(
            out.iter().copied().collect::<Vec<_>>(),
            vec![0x0041, 0x005A, 0x4EAC, 0x6771]
        );
        // Astral codepoints have no glyph the protocol can address.
        collect_codepoints("𝄞", &mut out);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn ranges_cover_each_codepoint_block_once() {
        let mut out = std::collections::BTreeSet::new();
        collect_codepoints("AZ", &mut out);
        collect_codepoints("東京", &mut out);
        // 'A' = U+0041 and 'Z' = U+005A share block 0; '東' = U+6771 →
        // 0x6700; '京' = U+4EAC → 0x4E00.
        let mut starts: Vec<u32> = out.iter().map(|cp| cp & !0xFF).collect();
        starts.dedup();
        assert_eq!(starts, vec![0x0000, 0x4E00, 0x6700]);
    }

    #[test]
    fn only_glyphs_sources_are_reported_as_fontstacks() {
        let doc = Document::from_json(
            r##"{
                "name": "test",
                "tile-size": 8,
                "sources": {
                    "labels": { "type": "glyphs",
                                "url": "https://x/{fontstack}/{range}.pbf",
                                "fontstack": "Noto Sans Regular" },
                    "face": { "type": "font", "url": "file:noto.ttf" }
                },
                "nodes": { "bg": { "op": "solid", "color": "#fff" } },
                "output": "bg"
            }"##,
        )
        .expect("style parses");

        let fields = |v: serde_json::Value| match v {
            serde_json::Value::Object(m) => m,
            _ => unreachable!(),
        };
        assert_eq!(
            glyph_source_names(
                &doc,
                &fields(serde_json::json!({ "font": ["labels", "face"] }))
            ),
            Some(vec!["labels"]),
            "a `font` source has no ranges to bind"
        );
        assert_eq!(
            glyph_source_names(&doc, &fields(serde_json::json!({ "font": "@labels" }))),
            Some(vec!["labels"]),
            "a `@` reference names the same source"
        );
        assert_eq!(
            glyph_source_names(&doc, &fields(serde_json::json!({ "color": "#fff" }))),
            None,
            "a node with no font stack is not a text node"
        );
    }
}
