//! WebAssembly bindings for the ezu painterly map renderer (Style).
//!
//! The JS side owns all I/O (HTTP, PMTiles, asset fetching). This crate
//! exposes a stateful [`Renderer`] that holds a parsed style document,
//! its built graph, an in-memory brush bank, and a per-tile binding
//! buffer that mirrors the style's `sources` block.
//!
//! The renderer itself is [`ezu_renderer`], which knows nothing about
//! JavaScript. What lives here is the translation: reading option
//! objects, building result objects, turning an
//! [`ezu_renderer::Error`] into a JS `Error`, throwing on a failed
//! allocation, and routing `tracing` into the console.
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
//! `PngEncode`, `WebpEncode`, `UnknownSource`. They are
//! [`ezu_renderer::ErrorKind`]'s names, so a Rust host and a JS host
//! see the same name for the same failure.
//!
//! `OutOfMemory` is thrown from wherever the heap ran out — see
//! [`oom`] for what the host may and may not do afterwards.

mod log;

pub use log::LogSink;

use ezu_renderer::{BindOptions, Error, ErrorKind, OutputFormat, PngCompression, RenderOptions};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

/// The name the allocator puts on a heap-exhaustion error. Read in a
/// `const` so throwing it needs no allocation.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const ERR_OOM: &str = ErrorKind::OutOfMemory.name();

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

/// Stateful WASM renderer.
#[wasm_bindgen]
pub struct Renderer {
    inner: ezu_renderer::Renderer,
}

#[wasm_bindgen]
impl Renderer {
    /// Build a renderer from a style JSON document.
    #[wasm_bindgen(constructor)]
    pub fn new(style_json: &str) -> Result<Renderer, JsValue> {
        #[cfg(feature = "panic-hook")]
        console_error_panic_hook::set_once();

        let inner = ezu_renderer::Renderer::new(style_json).map_err(js_err)?;
        Ok(Self { inner })
    }

    /// Replace the active style. Returns the new node count. Invalidates
    /// the intermediate cache and drops any pending source bindings.
    #[wasm_bindgen(js_name = setStyle)]
    pub fn set_style(&mut self, style_json: &str) -> Result<usize, JsValue> {
        self.inner.set_style(style_json).map_err(js_err)
    }

    /// `tile-size` declared by the current style.
    #[wasm_bindgen(getter, js_name = tileSize)]
    pub fn tile_size(&self) -> u32 {
        self.inner.tile_size()
    }

    /// JSON Schema for the current style's `params` — types, defaults,
    /// ranges, descriptions. The same document the CLI's tile server
    /// serves at `/style/params`. Generate sliders and colour pickers
    /// from this rather than parsing the style; it follows `setStyle`,
    /// so a params panel driven off it cannot drift from the graph
    /// being rendered.
    #[wasm_bindgen(getter, js_name = paramsSchema)]
    pub fn params_schema(&self) -> Result<JsValue, JsValue> {
        js_sys::JSON::parse(&self.inner.params_schema().to_string())
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
    /// `opts` is a JS object: `{ coord?: [dx, dy], sourceZoom?: number }`.
    ///
    /// `sourceZoom` declares that MVT bytes are natively encoded at a
    /// *shallower* zoom than the tile being rendered — a vector source
    /// that stops at its `maxzoom` while the host serves deeper tiles.
    /// The renderer then reprojects each payload from its own ancestor
    /// into the tile's frame before rendering (MVT "overzoom"), which is
    /// what a client would otherwise do by scaling a raster up. The
    /// ancestor is derived, not supplied: for a tile at zoom `z`, the
    /// ancestor at `sourceZoom` is unique, so a host that binds the
    /// `maxzoom` tile it fetched has nothing further to compute — and
    /// each neighbour resolves against its own ancestor, which for a 3×3
    /// window may be a different parent than the centre's.
    ///
    /// `sourceZoom` equal to the rendered zoom is accepted and does
    /// nothing, so a host can pass its source's `maxzoom` unconditionally
    /// and let shallow tiles take the ordinary path. Deeper than the
    /// rendered zoom throws `UnknownSource`: there is no way to invent
    /// detail the bytes do not carry.
    ///
    /// Throws `UnknownSource` if `name` doesn't match any entry in the
    /// style's `sources` block, `MvtDecode` if MVT bytes don't parse, and
    /// `DemDecode` for non-image DEM bytes (the decode itself runs at
    /// render time, but obvious cases are caught here).
    #[wasm_bindgen(js_name = bindSource)]
    pub fn bind_source(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
        opts: Option<js_sys::Object>,
    ) -> Result<(), JsValue> {
        let parsed = parse_bind_options(opts.as_ref())?;
        self.inner.bind_source(name, bytes, &parsed).map_err(js_err)
    }

    /// Effective attribution declared by the style (document +
    /// sources), joined with ` | `. Upstream TileJSON / PMTiles
    /// metadata is the JS host's concern — merge it on that side.
    #[wasm_bindgen(getter)]
    pub fn attribution(&self) -> Option<String> {
        self.inner.attribution()
    }

    /// Drop every pending source binding. Call between tile renders.
    #[wasm_bindgen(js_name = clearSources)]
    pub fn clear_sources(&mut self) {
        self.inner.clear_sources();
    }

    /// Cap the glyph bytes each bound fontstack keeps resident, in
    /// bytes. Unset, a fontstack keeps every range ever bound to it for
    /// the life of the renderer — `clearSources` does not touch glyphs,
    /// and on a long-lived instance rendering across a basemap that is
    /// usually what grew.
    ///
    /// Trimming happens **after** each `renderTile`, not while binding,
    /// so a render never loses glyphs that were bound for it. The tile
    /// that just drew is therefore the most recently used, and a budget
    /// large enough for one tile's glyphs always keeps that tile's; what
    /// goes is what earlier tiles needed and this one did not. Set it
    /// below one tile's worth and the ceiling still holds — the stack
    /// empties after every render and nothing carries over, which
    /// renders correctly but buys no reuse.
    ///
    /// It is a per-fontstack ceiling: a style with a regular, a medium
    /// and an italic stack can hold three times what is set here.
    ///
    /// This host cannot refetch, so anything trimmed must be bound again
    /// before the next tile that needs it. `neededCodepoints()` already
    /// names exactly what to bind, and a host that re-binds every tile
    /// (rather than tracking what it sent) needs no other change.
    ///
    /// A budget of `0xFFFFFFFF` — the largest value this host can express,
    /// since an address here is 32 bits — lifts the cap again, and is what
    /// `memoryUsage().glyphBudget` reports as `Infinity`. A number that
    /// big cannot be a real ceiling: it is every byte the module could
    /// ever address.
    #[wasm_bindgen(js_name = setGlyphBudget)]
    pub fn set_glyph_budget(&mut self, bytes: usize) {
        self.inner
            .set_glyph_budget((bytes != usize::MAX).then_some(bytes));
    }

    /// Every source the style declares, in declaration order, as an array
    /// of `{ name, type, tileScoped, url?, indexUrl? }`.
    ///
    /// This is what a host walks before it fetches anything: `type` is the
    /// style's own word for the kind (`"mvt"`, `"dem"`, `"glyphs"`, …) and
    /// says what `bindSource` will do with the bytes, and `url` is where
    /// they come from — an XYZ template for a tile pyramid, a document
    /// URL, or, for `glyphs`, the endpoint with `{fontstack}` already
    /// substituted and `{range}` left to fill in per block. `boundSources`
    /// answers the opposite question, what is already bound, and cannot
    /// start the loop.
    ///
    /// `tileScoped` says whether a binding of that kind is dropped by
    /// `clearSources` and has to be rebound for the next tile; the rest go
    /// into the persistent banks and are bound once.
    ///
    /// `url` is absent only for a `geojson` source with inline `data`: the
    /// style carries the payload, so there is nothing to fetch and nothing
    /// to bind. `indexUrl` appears on a `sprite` source whose index is a
    /// URL rather than inline — fetch it and pass the text as
    /// `bindSource(name, atlasBytes, { index })`.
    #[wasm_bindgen(js_name = sources)]
    pub fn sources(&self) -> Result<js_sys::Array, JsValue> {
        let out = js_sys::Array::new();
        for source in self.inner.sources() {
            let entry = js_sys::Object::new();
            js_sys::Reflect::set(&entry, &"name".into(), &JsValue::from_str(source.name))?;
            js_sys::Reflect::set(
                &entry,
                &"type".into(),
                &JsValue::from_str(source.kind.name()),
            )?;
            js_sys::Reflect::set(
                &entry,
                &"tileScoped".into(),
                &JsValue::from_bool(source.kind.is_tile_scoped()),
            )?;
            if let Some(url) = &source.url {
                js_sys::Reflect::set(&entry, &"url".into(), &JsValue::from_str(url))?;
            }
            if let Some(index_url) = source.index_url {
                js_sys::Reflect::set(&entry, &"indexUrl".into(), &JsValue::from_str(index_url))?;
            }
            out.push(&entry);
        }
        Ok(out)
    }

    /// Names of every source with at least one pending binding.
    /// Order matches the style's `sources` declaration order.
    #[wasm_bindgen(js_name = boundSources)]
    pub fn bound_sources(&self) -> Vec<String> {
        self.inner.bound_sources()
    }

    /// The style's declared `legend`, or `undefined` when it declares
    /// none. Pass a zoom to keep only the entries that apply there.
    ///
    /// Entries name the node that draws the symbol rather than restating
    /// a colour, so a host lays out the labels and asks the map itself
    /// for the swatches. The CLI's `ezu legend` renders those swatches;
    /// in a browser, render the named node.
    #[wasm_bindgen(js_name = legend)]
    pub fn legend(&self, zoom: Option<u8>) -> Result<JsValue, JsValue> {
        match self.inner.legend_json(zoom).map_err(js_err)? {
            Some(json) => js_sys::JSON::parse(&json),
            None => Ok(JsValue::UNDEFINED),
        }
    }

    /// The tile a host should actually fetch from `name` in order to draw
    /// `z/x/y`, as `{ z, x, y }`.
    ///
    /// For a source that declares `max-zoom`, a request past the ceiling
    /// answers with the covering ancestor — bind those bytes with
    /// `{ sourceZoom: <returned z> }` and the renderer resamples or
    /// reprojects them into the requested tile. Below the ceiling, and for
    /// a source that declares no ceiling, the answer is the tile itself.
    ///
    /// This exists so the ceiling lives in one place. A host that hard-codes
    /// the maxzoom of each source keeps a second copy of something the style
    /// already states, and the two drift.
    ///
    /// `x` and `y` may be off the tile grid, which is what a host walking
    /// a neighbourhood hands in: `x` wraps around the antimeridian, so
    /// the western neighbour of `z/0/y` is the real tile at the far side
    /// of the world. `y` does not wrap — there is no tile above the north
    /// pole — and an out-of-range row comes back unchanged, so the fetch
    /// for it misses and the stitch clamps that edge, which is what the
    /// pole should look like.
    ///
    /// Throws `UnknownSource` if `name` is not declared in the style.
    #[wasm_bindgen(js_name = sourceTile)]
    pub fn source_tile(&self, name: &str, z: u8, x: i32, y: i32) -> Result<JsValue, JsValue> {
        let (tz, tx, ty) = self.inner.source_tile(name, z, x, y).map_err(js_err)?;
        let out = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&out, &"z".into(), &JsValue::from(tz));
        let _ = js_sys::Reflect::set(&out, &"x".into(), &JsValue::from_f64(tx as f64));
        let _ = js_sys::Reflect::set(&out, &"y".into(), &JsValue::from_f64(ty as f64));
        Ok(out.into())
    }

    /// Neighbour tile offsets the active style actually asks for from
    /// `source`, as an array of `[dx, dy]` pairs (never including the
    /// centre `[0, 0]`).
    ///
    /// Cross-tile label collision and edge-continuous DEM/raster shading
    /// are the only things that read neighbours, and only for the sources
    /// they name. Pass each offset from this list to
    /// `bindSource(name, bytes, { coord })` and the render gets exactly
    /// what the recipe needs — neither a blind 3×3 per source nor, more
    /// costly, a window short of what a stitched source wanted. An empty
    /// array means the centre tile is enough.
    ///
    /// What comes back depends on the source's kind. A vector source
    /// answers from the graph: a node that wants a neighbour names it,
    /// and cross-tile label collision is usually the only thing that
    /// does, so the list is often empty. A `dem` or `raster` source
    /// answers from its own `neighbor-fetch`, which defaults to on and
    /// means the whole 3×3 — those bind as one stitched canvas, and a
    /// neighbour left unbound is not a missing collision candidate but a
    /// pad filled by clamping the tile's own edge, which shows up as a
    /// seam in whatever samples it. `neighbor-fetch: false` reports
    /// nothing whatever the graph asks for; so does a source no node in
    /// the graph reads.
    ///
    /// Throws `UnknownSource` if `name` is not declared in the style.
    #[wasm_bindgen(js_name = requestedNeighborOffsets)]
    pub fn requested_neighbor_offsets(&self, name: &str) -> Result<js_sys::Array, JsValue> {
        let offsets = self
            .inner
            .requested_neighbor_offsets(name)
            .map_err(js_err)?;
        let out = js_sys::Array::new();
        for (dx, dy) in offsets {
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
        glyph_units_object(self.inner.needed_codepoints().map_err(js_err)?)
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
    ///
    /// An Arabic letter contributes the presentation-form ranges as
    /// well as its own: it is drawn as whichever of its joined shapes
    /// the letters around it call for, and those shapes live in
    /// U+FB50‥U+FDFF and U+FE70‥U+FEFF.
    #[wasm_bindgen(js_name = neededGlyphRanges)]
    pub fn needed_glyph_ranges(&self) -> Result<js_sys::Object, JsValue> {
        glyph_units_object(self.inner.needed_glyph_ranges().map_err(js_err)?)
    }

    /// What this renderer is holding, in bytes, so a host can shed load
    /// *before* an allocation fails rather than after.
    ///
    /// Returns a JS object:
    /// - `heapBytes` — wasm linear memory committed to the **module**,
    ///   shared by every `Renderer` in this instance. This is the number
    ///   that meets an isolate's memory cap. It is a high-water mark:
    ///   freeing Rust values returns them to the allocator, never to the
    ///   host, so it only ever grows.
    /// - `glyphBytes` / `glyphRanges` — SDF bitmaps resident in the
    ///   glyph bank, and how many 256-codepoint blocks they span. Glyphs
    ///   accumulate for the life of the renderer and survive
    ///   `clearSources`, so on a long-lived instance this is usually
    ///   what grew. `glyphBudget` is the per-fontstack ceiling
    ///   `setGlyphBudget` put on them, or `Infinity` if none — note
    ///   `glyphBytes` totals *every* fontstack, so it can exceed the
    ///   budget legitimately.
    /// - `fontBytes` — outline font files held in the font bank.
    /// - `imageBytes` — decoded pixels of bound images and sprite
    ///   atlases.
    /// - `cacheBytes` / `cacheBudget` — the render cache's pixel
    ///   payload against its own eviction budget; it bounds itself, so
    ///   `cacheBytes` near `cacheBudget` is steady state, not a leak.
    ///
    /// These are payload sizes, not an accounting of the heap: they omit
    /// allocator overhead, decoded features, per-font glyph-path caches,
    /// and the buffers a render is using right now. Expect the parts to
    /// sum to less than `heapBytes`.
    #[wasm_bindgen(js_name = memoryUsage)]
    pub fn memory_usage(&self) -> Result<js_sys::Object, JsValue> {
        let usage = self.inner.memory_usage();
        let out = js_sys::Object::new();
        let set = |key: &str, value: usize| -> Result<(), JsValue> {
            js_sys::Reflect::set(
                &out,
                &JsValue::from_str(key),
                &JsValue::from_f64(value as f64),
            )?;
            Ok(())
        };
        set("heapBytes", heap_bytes())?;
        set("glyphBytes", usage.glyph_bytes)?;
        set("glyphRanges", usage.glyph_ranges)?;
        // An unset budget reads as `Infinity`, not as the bewildering
        // 1.8e19 that `usize::MAX` would land on.
        js_sys::Reflect::set(
            &out,
            &JsValue::from_str("glyphBudget"),
            &JsValue::from_f64(usage.glyph_budget.map_or(f64::INFINITY, |b| b as f64)),
        )?;
        set("fontBytes", usage.font_bytes)?;
        set("imageBytes", usage.image_bytes)?;
        set("cacheBytes", usage.cache_bytes)?;
        set("cacheBudget", usage.cache_budget)?;
        Ok(out)
    }

    /// Render a single tile using whatever sources are currently bound.
    ///
    /// `opts` (JS object, all fields optional):
    /// - `format`: `"png"` (default) / `"webp"` / `"rgba"`
    /// - `tileSize`, `pad`: override the style's canvas size for this
    ///   call (hi-DPI / preview)
    /// - `params`: `{ name: number | boolean | string }` — render-time
    ///   overrides for the style's declared `params`, validated the same
    ///   way the CLI's `--param` is. Omitted names keep their declared
    ///   default.
    /// - `png`: `{ compression?: "fast" | "default" | "best" }`
    #[wasm_bindgen(js_name = renderTile)]
    pub fn render_tile(
        &self,
        z: u8,
        x: u32,
        y: u32,
        opts: Option<js_sys::Object>,
    ) -> Result<Vec<u8>, JsValue> {
        let parsed = parse_render_options(opts.as_ref())?;
        self.inner.render_tile(z, x, y, parsed).map_err(js_err)
    }
}

/// `{ [glyphsSourceName]: number[] }` from the prepass's answer.
fn glyph_units_object(
    units_by_source: std::collections::BTreeMap<&str, Vec<u32>>,
) -> Result<js_sys::Object, JsValue> {
    let out = js_sys::Object::new();
    for (source, units) in units_by_source {
        let arr = js_sys::Array::new();
        for u in units {
            arr.push(&JsValue::from(u));
        }
        js_sys::Reflect::set(&out, &JsValue::from_str(source), &arr)?;
    }
    Ok(out)
}

/// Parse the `renderTile` options object. Unknown keys are silently
/// ignored so future fields stay backwards-compatible.
fn parse_render_options(obj: Option<&js_sys::Object>) -> Result<RenderOptions, JsValue> {
    let mut out = RenderOptions::default();
    let Some(obj) = obj else {
        return Ok(out);
    };
    // format: "png" | "webp" | "rgba"
    if let Some(s) = js_sys::Reflect::get(obj, &"format".into())
        .ok()
        .and_then(|v| v.as_string())
    {
        // An unrecognised name is a typo, not a request for the default:
        // silently answering PNG to `format: "jpeg"` hands back bytes the
        // caller will not be expecting.
        out.format = match s.as_str() {
            "png" => OutputFormat::Png,
            "webp" => OutputFormat::Webp,
            "rgba" => OutputFormat::Rgba,
            other => {
                return Err(named_err(
                    ErrorKind::InvalidStyle,
                    format!("format must be \"png\", \"webp\" or \"rgba\", got \"{other}\""),
                ))
            }
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
    // params: { name: number | boolean | string }
    let params = js_sys::Reflect::get(obj, &"params".into()).unwrap_or(JsValue::UNDEFINED);
    if let Some(params_obj) = params.dyn_ref::<js_sys::Object>() {
        for key in js_sys::Object::keys(params_obj).iter() {
            let Some(name) = key.as_string() else {
                continue;
            };
            let value = js_sys::Reflect::get(params_obj, &key).unwrap_or(JsValue::UNDEFINED);
            // Numbers and bools are stringified rather than matched on:
            // `parse_param_value` owns the coercion rules, and going
            // through text keeps this host from inventing its own.
            let raw = if let Some(s) = value.as_string() {
                s
            } else if let Some(n) = value.as_f64() {
                let mut t = n.to_string();
                if t.ends_with(".0") {
                    t.truncate(t.len() - 2);
                }
                t
            } else if let Some(b) = value.as_bool() {
                b.to_string()
            } else {
                continue;
            };
            out.params.push((name, raw));
        }
    }

    let png = js_sys::Reflect::get(obj, &"png".into()).unwrap_or(JsValue::UNDEFINED);
    if let Some(png_obj) = png.dyn_ref::<js_sys::Object>() {
        if let Some(s) = js_sys::Reflect::get(png_obj, &"compression".into())
            .ok()
            .and_then(|v| v.as_string())
        {
            out.png_compression = match s.as_str() {
                "fast" => PngCompression::Fast,
                "default" => PngCompression::Default,
                "best" => PngCompression::Best,
                other => {
                    return Err(named_err(
                        ErrorKind::InvalidStyle,
                        format!(
                            "png.compression must be \"fast\", \"default\" or \"best\",                              got \"{other}\""
                        ),
                    ))
                }
            };
        }
    }
    Ok(out)
}

/// Parse the `bindSource` options object:
/// `{ coord?: [dx, dy], sourceZoom?: number, index?: string }`.
fn parse_bind_options(obj: Option<&js_sys::Object>) -> Result<BindOptions, JsValue> {
    Ok(BindOptions {
        coord: parse_coord_opt(obj)?,
        source_zoom: parse_source_zoom_opt(obj)?,
        index: parse_index_opt(obj),
    })
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
    let arr = coord.dyn_into::<js_sys::Array>().map_err(|_| {
        named_err(
            ErrorKind::UnknownSource,
            "coord must be a [dx, dy] array of two numbers",
        )
    })?;
    if arr.length() != 2 {
        return Err(named_err(
            ErrorKind::UnknownSource,
            "coord must have exactly two numbers [dx, dy]",
        ));
    }
    let dx = arr
        .get(0)
        .as_f64()
        .ok_or_else(|| named_err(ErrorKind::UnknownSource, "coord[0] (dx) must be a number"))?
        as i32;
    let dy = arr
        .get(1)
        .as_f64()
        .ok_or_else(|| named_err(ErrorKind::UnknownSource, "coord[1] (dy) must be a number"))?
        as i32;
    // Only the 3×3 neighbourhood is ever stitched or collided against, so
    // anything further out would be accepted, stored, and never read.
    if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) {
        return Err(named_err(
            ErrorKind::UnknownSource,
            format!("coord [{dx}, {dy}] is outside the 3×3 neighbourhood (dx, dy ∈ -1..=1)"),
        ));
    }
    Ok((dx, dy))
}

/// Read `sourceZoom` off a `bindSource` options object: the zoom the
/// bound bytes are natively encoded at, when it is shallower than the
/// tile being rendered.
fn parse_source_zoom_opt(obj: Option<&js_sys::Object>) -> Result<Option<u8>, JsValue> {
    let Some(obj) = obj else {
        return Ok(None);
    };
    let value = js_sys::Reflect::get(obj, &"sourceZoom".into()).unwrap_or(JsValue::UNDEFINED);
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    let z = value
        .as_f64()
        .ok_or_else(|| named_err(ErrorKind::UnknownSource, "sourceZoom must be a number"))?;
    if !z.is_finite() || z.fract() != 0.0 || !(0.0..=30.0).contains(&z) {
        return Err(named_err(
            ErrorKind::UnknownSource,
            format!("sourceZoom must be a whole zoom level in 0..=30, got {z}"),
        ));
    }
    Ok(Some(z as u8))
}

/// Whether the wasm binary was compiled with `+simd128`. Lets the demo
/// page label the renderer build accurately.
#[wasm_bindgen(js_name = simdEnabled)]
pub fn simd_enabled() -> bool {
    cfg!(target_feature = "simd128")
}

/// Wasm linear memory currently committed to this module, in bytes.
///
/// This is the figure an isolate's memory cap applies to, and the one
/// to watch to shed load before an allocation fails — a refused
/// `memory.grow` throws [`OutOfMemory`](oom) and ends the instance.
/// It never falls: freed Rust values return to the allocator for reuse,
/// but wasm cannot hand pages back to the host. So a drop in demand
/// leaves the number where its peak left it, and only a fresh instance
/// resets it.
///
/// Module-wide, not per-`Renderer`. For what a given renderer is
/// holding, and which bank to evict, call `memoryUsage()`.
#[wasm_bindgen(js_name = heapBytes)]
pub fn heap_bytes() -> usize {
    #[cfg(target_arch = "wasm32")]
    {
        // `memory_size` counts 64 KiB pages of memory 0.
        core::arch::wasm32::memory_size(0) * 65536
    }
    // Native builds (tests, docs) have no linear memory to report.
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
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

/// Turn a renderer failure into a JS `Error` whose `.name` discriminates
/// the failure kind so callers can dispatch on it.
fn js_err(e: Error) -> JsValue {
    named_err(e.kind(), e)
}

/// Build a JS `Error` whose `.name` discriminates the failure kind, for
/// the failures this shell raises on its own — malformed option objects,
/// which never reach the renderer.
fn named_err(kind: ErrorKind, e: impl std::fmt::Display) -> JsValue {
    let err = js_sys::Error::new(&e.to_string());
    err.set_name(kind.name());
    err.into()
}
