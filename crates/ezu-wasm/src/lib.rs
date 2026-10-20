//! WebAssembly bindings for the ezu painterly map renderer (Style).
//!
//! The JS side owns all I/O (HTTP, PMTiles, asset fetching). This crate
//! exposes a stateful [`Renderer`] that holds a parsed style document,
//! its built graph, an in-memory brush bank, and a per-tile binding
//! buffer that mirrors the style's `sources` block.
//!
//! The renderer itself is [`ezu::renderer`], which knows nothing about
//! JavaScript. What lives here is the translation: reading option
//! objects, building result objects, turning an
//! [`ezu::renderer::Error`] into a JS `Error`, throwing on a failed
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
//! ## Rendering from loaded data
//!
//! Binding is state to reset between tiles, and it decodes the bytes each
//! time they are bound. `loadSource` decodes a tile once into a
//! `SourceData`; a `SourceSet` says which of them one render reads, and
//! where; `renderTileWith` renders from it and from nothing that is bound.
//! A tile several renders read — the centre of one, a neighbour of the next
//! — is loaded once and put into each of their sets:
//!
//! ```js
//! const data = r.loadSource("basemap", await fetchMvt(z, x, y));
//! const set = new SourceSet();
//! set.bind(data);                        // takes bindSource's { coord, sourceZoom }
//! const png = r.renderTileWith(z, x, y, set);
//! ```
//!
//! ## Output
//!
//! `renderTile(z, x, y, opts?)` returns bytes in one of three formats
//! selected by `opts.format`: `"png"` (default), `"webp"` (lossless),
//! or `"rgba"` (straight un-premultiplied 8-bit RGBA, feed directly
//! into `ctx.putImageData`). `opts.tileSize` / `opts.pad` override the
//! style's canvas size for hi-DPI previews.
//!
//! ## Print frames
//!
//! `computeFrame(req)` turns a scale, dpi, page size and centre into the
//! zoom, tile size and tiles `ezu frame` would render, and
//! `renderFrame(frame, sources)` renders them — awaiting from `sources` the
//! `SourceSet` each tile renders from — into one straight-RGBA page, the
//! CLI's pixel for pixel. While it runs the renderer is the frame's:
//! glyphs a tile needs are bound through the binder `sources` is handed,
//! and changing the renderer from anywhere else throws `FrameInProgress`.
//! `setTileSize` / `clearTileSize` render at another tile size for a host
//! that renders a frame's tiles itself.
//!
//! ## Errors
//!
//! All fallible methods throw a JavaScript `Error` whose `.name`
//! discriminates the failure kind: `InvalidStyle`, `BrushParse`,
//! `MvtDecode`, `DemDecode`, `RasterDecode`, `GeoJsonDecode`,
//! `SpriteDecode`, `FontParse`, `GlyphDecode`, `RenderFailed`,
//! `PngEncode`, `WebpEncode`, `UnknownSource`. They are
//! [`ezu::renderer::ErrorKind`]'s names, so a Rust host and a JS host
//! see the same name for the same failure.
//!
//! Two names are this shell's own: `FrameInProgress`, for a call that would change a
//! renderer a `renderFrame` holds, and `StaleBinder`, for a frame's binder
//! used after its tile's `sources` has finished.
//!
//! `OutOfMemory` is thrown from wherever the heap ran out — see
//! the `oom` module for what the host may and may not do afterwards.

mod log;

pub use log::LogSink;

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use ezu::renderer::{
    BindOptions, Error, ErrorKind, FeatureCoords, Frame, FrameFeatureNode, FrameFeatures,
    FrameRequest, OutputFormat, PngCompression, RenderOptions,
};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

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
///
/// Every method takes `&self` and borrows the renderer inside, rather than
/// letting wasm-bindgen hold a borrow for the length of the call:
/// `renderFrame` awaits JS between its tiles, and the loads and read-only
/// queries a tile's `sources` makes (`loadSource`, `sourceTile`, …) have to
/// reach the same
/// renderer while the frame is still rendering. The `Rc` is what the
/// frame's promise keeps alive between tiles.
#[wasm_bindgen]
pub struct Renderer {
    inner: Rc<RefCell<ezu::renderer::Renderer>>,
    frame: Rc<FrameState>,
}

/// Who may change the renderer while `renderFrame` runs.
///
/// A frame awaits its `sources` between tiles, and other JS runs during
/// that await — another request on the same Worker, a timer — so a
/// `bindSource` arriving then could be the frame's or anybody's, and the
/// call stack no longer says which. The frame therefore takes the renderer
/// for itself (`busy`) and hands each tile's `sources` a [`FrameBinder`]
/// carrying that tile's number; only a binder whose number is the one
/// `open_tile` holds binds anything.
#[derive(Default)]
struct FrameState {
    busy: Cell<bool>,
    /// The tile whose `sources` is running, or 0 between and outside them.
    open_tile: Cell<u64>,
    /// Tile numbers handed out so far, so no two binders ever share one.
    issued: Cell<u64>,
}

#[wasm_bindgen]
impl Renderer {
    /// Build a renderer from a style JSON document.
    #[wasm_bindgen(constructor)]
    pub fn new(style_json: &str) -> Result<Renderer, JsValue> {
        #[cfg(feature = "panic-hook")]
        console_error_panic_hook::set_once();

        let inner = ezu::renderer::Renderer::new(style_json).map_err(js_err)?;
        Ok(Self {
            inner: Rc::new(RefCell::new(inner)),
            frame: Rc::default(),
        })
    }

    /// Replace the active style. Returns the new node count. Invalidates
    /// the intermediate cache, drops any pending source bindings, and
    /// goes back to the new style's own tile size.
    #[wasm_bindgen(js_name = setStyle)]
    pub fn set_style(&self, style_json: &str) -> Result<usize, JsValue> {
        self.idle()?;
        self.inner
            .borrow_mut()
            .set_style(style_json)
            .map_err(js_err)
    }

    /// `tile-size` declared by the current style, whatever `setTileSize`
    /// renders at.
    #[wasm_bindgen(getter, js_name = tileSize)]
    pub fn tile_size(&self) -> u32 {
        self.inner.borrow().tile_size()
    }

    /// Render every tile at `size` px instead of the style's `tile-size`,
    /// without editing the style. `clearTileSize()` goes back to the
    /// style's, and so does `setStyle`.
    ///
    /// A print frame needs it: `computeFrame` meets the scale by choosing
    /// the tile size, and the frame's tiles must be drawn at exactly that
    /// size. It renders what the style with its `tile-size` edited would.
    /// Nothing you bind depends on it — `requestedNeighborOffsets` and
    /// `sourceTile` answer as before — and the intermediate cache, which is
    /// sized by the tile, is dropped when the size changes. `tileSize`
    /// keeps reporting the style's own, and a `tileSize` in one
    /// `renderTile`'s options still wins over this.
    ///
    /// Throws `InvalidStyle` for a size below 1 px: `0` is not "the
    /// style's", that is `clearTileSize`.
    #[wasm_bindgen(js_name = setTileSize)]
    pub fn set_tile_size(&self, size: f64) -> Result<(), JsValue> {
        self.idle()?;
        let size = whole_pixels(size, "tile size")?;
        if size == 0 {
            return Err(named_err(
                ErrorKind::InvalidStyle,
                "a tile size must be at least 1 px; clearTileSize() goes back to the style's",
            ));
        }
        self.inner
            .borrow_mut()
            .set_tile_size(Some(size))
            .map_err(js_err)
    }

    /// Go back to rendering at the style's own `tile-size`, undoing
    /// `setTileSize`.
    #[wasm_bindgen(js_name = clearTileSize)]
    pub fn clear_tile_size(&self) -> Result<(), JsValue> {
        self.idle()?;
        self.inner.borrow_mut().set_tile_size(None).map_err(js_err)
    }

    /// Where a print frame sits on the tile grid: the zoom and tile size
    /// that meet `req`'s scale, and the tiles that cover it — the
    /// arithmetic `ezu frame` does before it renders anything, done by the
    /// same code, so a frame computed here is the one the CLI would render.
    ///
    /// `req` is `{ centerLon, centerLat, scale, dpi, width, height, zoom?,
    /// styleTileSize? }`: `scale` is the denominator (1000 for 1:1000),
    /// `width` and `height` the output size in pixels. `zoom` fixes the
    /// zoom, as the CLI's `--zoom` does; left out, the zoom chosen keeps
    /// the tile size nearest `styleTileSize`, which defaults to this
    /// renderer's style.
    ///
    /// The answer is `{ zoom, tileSize, scale, width, height, left, top,
    /// shift: [x, y], tilesX: [start, end], tilesY: [start, end] }`. Hand
    /// it to `renderFrame`, or render tiles `tilesX × tilesY` (end
    /// exclusive) at `zoom` and `tileSize` yourself: tile `(x, y)` lands
    /// with its top-left at `(x · tileSize − left, y · tileSize − top)`,
    /// cropped to `width × height`. `scale` is the denominator actually
    /// met, and `shift` how far rounding the frame onto whole pixels moved
    /// it.
    ///
    /// It changes nothing on the renderer. A request out of its domain (a
    /// non-positive scale, a latitude past the Mercator edge, a zero-sized
    /// output), a frame across the antimeridian, and a scale that needs a
    /// tile size outside 16..8192 px all throw `InvalidStyle`.
    #[wasm_bindgen(js_name = computeFrame, unchecked_return_type = "Frame")]
    pub fn compute_frame(
        &self,
        #[wasm_bindgen(unchecked_param_type = "FrameRequest")] req: js_sys::Object,
    ) -> Result<JsValue, JsValue> {
        let inner = self.inner.borrow();
        let req = parse_frame_request(&req, inner.tile_size())?;
        let frame = inner.compute_frame(&req).map_err(js_err)?;
        frame_object(&frame)
    }

    /// Render every tile `frame` covers and copy each one's share into one
    /// frame-sized buffer, which is what `ezu frame` does: the same tiles,
    /// at the same tile size, cropped the same way. Resolves to straight
    /// (un-premultiplied) RGBA, `width × height × 4` bytes, as
    /// `format: "rgba"` is.
    ///
    /// The tile data stays yours to fetch, tile by tile, but it is handed
    /// over as a value rather than bound: before each tile
    /// `sources({ z, x, y }, binder)` is called with the tile about to
    /// render, and the tile renders from the `SourceSet` it returns, as
    /// `renderTileWith` does — the tile itself, the neighbours
    /// `requestedNeighborOffsets` names, the ancestor `sourceTile` answers
    /// past a source's max-zoom. A frame's tiles border each other, so keep
    /// the `SourceData` `loadSource` gives you and put each one into the set
    /// of every tile that reads it: each fetched tile is then decoded once,
    /// not once per tile around it. The frame reads the set and leaves it
    /// yours; returning one set for several tiles is fine.
    ///
    /// `sources` may return a promise, which is awaited, so it can fetch.
    /// One that throws or rejects stops the frame with that error. It may
    /// be `null` for a style that needs no tile data. The binder binds into
    /// the renderer what a set turns out to need — the glyphs
    /// `neededCodepointsIn` lists for it — with `binder.bindSource`, which
    /// takes what `bindSource` does but refuses a tile-scoped source with
    /// `UnknownSource`, since the frame never reads bound tile data.
    /// Persistent sources (fonts, sprites, brushes, images) can equally be
    /// bound once, before the call. After each tile the glyph banks are
    /// trimmed to `setGlyphBudget`'s budget, as `renderTile` does.
    ///
    /// Until the promise settles the renderer is the frame's: everything
    /// that would change what it draws — `bindSource`, `clearSources`,
    /// `renderTile`, `trimGlyphs`, another `renderFrame` or
    /// `renderFrameWithFeatures`, `setStyle`, `setTileSize`,
    /// `clearTileSize`, `setGlyphBudget`, `setCacheBudget`,
    /// `clearCacheBudget` — throws `FrameInProgress`, so a request running while `sources` awaits
    /// cannot change the frame's tiles under it. `loadSource`,
    /// `renderTileWith` and the queries that only read (`sourceTile`,
    /// `requestedNeighborOffsets`, `neededCodepointsIn`, `boundSources`, …)
    /// still work. A binder binds only for its own tile and only while that
    /// tile's `sources` runs; kept and used later, it throws `StaleBinder`.
    /// The renderer is given back however the frame ends, and the frame
    /// binds no tile data, so it leaves none bound.
    ///
    /// The tiles render at `frame.tileSize` per render, so the renderer's
    /// own tile size is left as it was. `opts` takes `renderTile`'s
    /// `pad` and `params`; its `format` and `tileSize` must be left out
    /// (or match), since the frame fixes both.
    ///
    /// Tiles render one after another, row by row from the top.
    #[wasm_bindgen(js_name = renderFrame, unchecked_return_type = "Promise<Uint8Array>")]
    pub fn render_frame(
        &self,
        #[wasm_bindgen(unchecked_param_type = "Frame")] frame: js_sys::Object,
        #[wasm_bindgen(
            unchecked_param_type = "((tile: { z: number; x: number; y: number }, binder: FrameBinder) => SourceSet | Promise<SourceSet>) | null | undefined"
        )]
        sources: Option<js_sys::Function>,
        opts: Option<js_sys::Object>,
    ) -> js_sys::Promise {
        if let Err(e) = self.idle() {
            return js_sys::Promise::reject(&e);
        }
        // Taken here, before the promise's first poll, so a call made
        // right after this one already finds the renderer busy.
        self.frame.busy.set(true);
        let (inner, state) = (Rc::clone(&self.inner), Rc::clone(&self.frame));
        wasm_bindgen_futures::future_to_promise(async move {
            let result = render_frame_tiles(&inner, &state, &frame, sources, opts, None).await;
            // Give the renderer back whether or not the frame made it.
            state.open_tile.set(0);
            state.busy.set(false);
            result.map(Into::into)
        })
    }

    /// `renderFrame`, also handing back what features nodes held over the
    /// whole page: resolves to `{ image, features }`, where `image` is the
    /// buffer `renderFrame` resolves to and `features` holds one
    /// `{ node, geojson }` per entry of `request.nodes`, in that order.
    /// `geojson` is a FeatureCollection as text, byte for byte the file
    /// `ezu frame --features` writes for the same style, data and frame.
    ///
    /// `request` is `{ nodes: [{ node, key? }], coords? }`. `key` names the
    /// properties that identify a feature across tiles — `["label-id",
    /// "part"]` for `label-boxes`: features whose key properties are all
    /// equal are written once, the first in reading order (rows top to
    /// bottom, then left to right), and the page is sorted by the key.
    /// Without one, every tile's features are written, so a feature several
    /// tiles see appears once per tile. `coords` is `"lonlat"` (the
    /// default), WGS84 to seven decimal places, or `"px"`, page pixels to
    /// two. Features wholly off the page are left out; one that reaches
    /// into it is written whole.
    ///
    /// The values are the ones each tile's render computed on its way to
    /// the pixels, so this costs no second evaluation. A name that is not
    /// a node of the style, or names one that produces something other
    /// than features, and a key that names no property, reject with
    /// `InvalidStyle` before anything renders.
    #[wasm_bindgen(js_name = renderFrameWithFeatures, unchecked_return_type = "Promise<FrameWithFeatures>")]
    pub fn render_frame_with_features(
        &self,
        #[wasm_bindgen(unchecked_param_type = "Frame")] frame: js_sys::Object,
        #[wasm_bindgen(
            unchecked_param_type = "((tile: { z: number; x: number; y: number }, binder: FrameBinder) => SourceSet | Promise<SourceSet>) | null | undefined"
        )]
        sources: Option<js_sys::Function>,
        #[wasm_bindgen(unchecked_param_type = "FrameFeatures")] request: js_sys::Object,
        opts: Option<js_sys::Object>,
    ) -> js_sys::Promise {
        if let Err(e) = self.idle() {
            return js_sys::Promise::reject(&e);
        }
        let collector = parse_frame(&frame)
            .and_then(|f| Ok((f, parse_frame_features(&request)?)))
            .and_then(|(f, req)| self.inner.borrow().frame_features(&f, &req).map_err(js_err));
        let mut collector = match collector {
            Ok(c) => c,
            Err(e) => return js_sys::Promise::reject(&e),
        };
        self.frame.busy.set(true);
        let (inner, state) = (Rc::clone(&self.inner), Rc::clone(&self.frame));
        wasm_bindgen_futures::future_to_promise(async move {
            let result =
                render_frame_tiles(&inner, &state, &frame, sources, opts, Some(&mut collector))
                    .await;
            state.open_tile.set(0);
            state.busy.set(false);
            let image = result?;
            let out = js_sys::Object::new();
            js_sys::Reflect::set(&out, &"image".into(), &image)?;
            let features = page_list(collector)?;
            js_sys::Reflect::set(&out, &"features".into(), &features)?;
            Ok(out.into())
        })
    }

    /// Start gathering what features nodes hold over `frame`'s page, for a
    /// host that renders the frame's tiles itself — with `tileSize` set to
    /// the frame's, say — rather than through `renderFrameWithFeatures`,
    /// which makes and drives one of its own. `request` is
    /// `renderFrameWithFeatures`'s `{ nodes: [{ node, key? }], coords? }`.
    /// Render each of the frame's tiles with the collector's `renderTile`,
    /// in any order, then `finish()`.
    ///
    /// It changes nothing about what the renderer draws, so it works while
    /// a `renderFrame` holds the renderer. Throws `InvalidStyle` for what
    /// `renderFrameWithFeatures` refuses, here, before anything renders.
    #[wasm_bindgen(js_name = frameFeatures)]
    pub fn frame_features(
        &self,
        #[wasm_bindgen(unchecked_param_type = "Frame")] frame: js_sys::Object,
        #[wasm_bindgen(unchecked_param_type = "FrameFeatures")] request: js_sys::Object,
    ) -> Result<FrameFeatureCollector, JsValue> {
        let frame = parse_frame(&frame)?;
        let request = parse_frame_features(&request)?;
        let inner = self
            .inner
            .borrow()
            .frame_features(&frame, &request)
            .map_err(js_err)?;
        Ok(FrameFeatureCollector {
            renderer: Rc::clone(&self.inner),
            inner,
        })
    }

    /// JSON Schema for the current style's `params` — types, defaults,
    /// ranges, descriptions. The same document the CLI's tile server
    /// serves at `/style/params`. Generate sliders and colour pickers
    /// from this rather than parsing the style; it follows `setStyle`,
    /// so a params panel driven off it cannot drift from the graph
    /// being rendered.
    #[wasm_bindgen(getter, js_name = paramsSchema)]
    pub fn params_schema(&self) -> Result<JsValue, JsValue> {
        js_sys::JSON::parse(&self.inner.borrow().params_schema().to_string())
    }

    /// Bind raw tile bytes under a `sources.<name>` entry from the style.
    /// The renderer dispatches on the source's declared `type`:
    ///
    /// - `mvt` / `pmtiles` → store raw MVT bytes per `(dx, dy)` neighbour
    ///   offset (centre `coord: [0, 0]`, the default). Layers are bound as
    ///   `<source>.<layer>` (centre) or `<source>.<layer>@dx,dy`
    ///   (neighbours) when [`render_tile`](Self::render_tile) runs. Binding only the centre
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
        &self,
        name: &str,
        bytes: Vec<u8>,
        opts: Option<js_sys::Object>,
    ) -> Result<(), JsValue> {
        self.idle()?;
        bind_into(&self.inner, name, bytes, opts.as_ref())
    }

    /// Effective attribution declared by the style (document +
    /// sources), joined with ` | `. Upstream TileJSON / PMTiles
    /// metadata is the JS host's concern — merge it on that side.
    #[wasm_bindgen(getter)]
    pub fn attribution(&self) -> Option<String> {
        self.inner.borrow().attribution()
    }

    /// Drop every pending source binding. Call between tile renders.
    #[wasm_bindgen(js_name = clearSources)]
    pub fn clear_sources(&self) -> Result<(), JsValue> {
        self.idle()?;
        self.inner.borrow_mut().clear_sources();
        Ok(())
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
    pub fn set_glyph_budget(&self, bytes: usize) -> Result<(), JsValue> {
        self.idle()?;
        self.inner
            .borrow_mut()
            .set_glyph_budget((bytes != usize::MAX).then_some(bytes));
        Ok(())
    }

    /// Let the render cache hold up to `bytes` of pixels instead of its
    /// default (8 MiB). The cache keeps intermediate results a later render
    /// can reuse — most of all what a world-anchored node computes, which
    /// every tile around it reads too — so a host rendering many
    /// neighbouring tiles with memory to spare renders them faster with
    /// more, and one in a memory-capped isolate can hold it to less.
    /// `memoryUsage().cacheBudget` reports it back.
    ///
    /// The cache starts empty at the new size, and keeps the size through
    /// `setStyle` and `setTileSize`. Throws `FrameInProgress` while a
    /// `renderFrame` holds the renderer.
    #[wasm_bindgen(js_name = setCacheBudget)]
    pub fn set_cache_budget(&self, bytes: usize) -> Result<(), JsValue> {
        self.idle()?;
        self.inner.borrow_mut().set_cache_budget(Some(bytes));
        Ok(())
    }

    /// Go back to the render cache's default size. Throws
    /// `FrameInProgress` while a `renderFrame` holds the renderer.
    #[wasm_bindgen(js_name = clearCacheBudget)]
    pub fn clear_cache_budget(&self) -> Result<(), JsValue> {
        self.idle()?;
        self.inner.borrow_mut().set_cache_budget(None);
        Ok(())
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
        let inner = self.inner.borrow();
        for source in inner.sources() {
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

    /// What building the current style's graph warned about: each names a
    /// node that works but relies on something the style does not say out
    /// loud. They are also logged when the style is built; this is for
    /// reporting or checking them on your own terms. Follows `setStyle`.
    #[wasm_bindgen(getter)]
    pub fn warnings(&self) -> Vec<String> {
        self.inner.borrow().warnings().to_vec()
    }

    /// Names of every source with at least one pending binding.
    /// Order matches the style's `sources` declaration order.
    #[wasm_bindgen(js_name = boundSources)]
    pub fn bound_sources(&self) -> Vec<String> {
        self.inner.borrow().bound_sources()
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
        match self.inner.borrow().legend_json(zoom).map_err(js_err)? {
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
        let (tz, tx, ty) = self
            .inner
            .borrow()
            .source_tile(name, z, x, y)
            .map_err(js_err)?;
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
            .borrow()
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
        glyph_units_object(self.inner.borrow().needed_codepoints().map_err(js_err)?)
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
        glyph_units_object(self.inner.borrow().needed_glyph_ranges().map_err(js_err)?)
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
        let usage = self.inner.borrow().memory_usage();
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
        self.idle()?;
        let parsed = parse_render_options(opts.as_ref())?;
        self.inner
            .borrow()
            .render_tile(z, x, y, parsed)
            .map_err(js_err)
    }

    /// Decode a tile-scoped source's bytes — `mvt`, `pmtiles`, `dem`,
    /// `raster`, or a remote `geojson` document — once, into a
    /// `SourceData` that any number of `SourceSet`s can hold without
    /// decoding it again.
    ///
    /// A host rendering a block of tiles loads each tile it fetched once
    /// and binds it into the set of every tile it borders, rather than
    /// decoding it as the centre of one tile and again as a neighbour of
    /// each of the others. Nothing about what the renderer draws changes,
    /// so this works while a `renderFrame` holds the renderer, which is
    /// where a frame's callback loads its tiles.
    ///
    /// Throws `UnknownSource` if `name` is not declared, or is a kind bound
    /// into the renderer itself with `bindSource` (brush, image, sprite,
    /// font, glyphs), and the matching decode kind if the bytes do not
    /// parse. Unlike `bindSource`, a DEM or imagery tile is decoded here,
    /// so its decode error is thrown here and not from the render.
    #[wasm_bindgen(js_name = loadSource)]
    pub fn load_source(&self, name: &str, bytes: &[u8]) -> Result<SourceData, JsValue> {
        let inner = self
            .inner
            .borrow()
            .load_source(name, bytes)
            .map_err(js_err)?;
        Ok(SourceData { inner })
    }

    /// Render a single tile from the tile data in `set`, as `renderTile`
    /// does from what is bound. Nothing tile-scoped that `bindSource` bound
    /// is read; the fonts, sprites, brushes, images and glyphs bound into
    /// the renderer are. `opts` is `renderTile`'s.
    ///
    /// It changes nothing a later render depends on, so it needs no
    /// `clearSources` and works while a `renderFrame` holds the renderer.
    /// It does not bring the glyph banks back under a budget set with
    /// `setGlyphBudget`, as `renderTile` does after each render; call
    /// `trimGlyphs()` when a batch of renders is done.
    #[wasm_bindgen(js_name = renderTileWith)]
    pub fn render_tile_with(
        &self,
        z: u8,
        x: u32,
        y: u32,
        set: &SourceSet,
        opts: Option<js_sys::Object>,
    ) -> Result<Vec<u8>, JsValue> {
        let parsed = parse_render_options(opts.as_ref())?;
        self.inner
            .borrow()
            .render_tile_with(z, x, y, &set.inner, parsed)
            .map_err(js_err)
    }

    /// `renderTileWith`, also handing back what each of `nodes` — ids of
    /// nodes that produce features — held when the render finished, in the
    /// order given: `{ image, features }`, where `image` is the bytes
    /// `renderTileWith` returns and `features` holds one
    /// `{ node, extent, geojson }` per name, `geojson` being a GeoJSON
    /// FeatureCollection as text — the shape `renderFrameWithFeatures` and
    /// the Go package hand back too.
    ///
    /// Each entry carries the node's id as `node` and the tile's size
    /// in geometry units as `extent`: coordinates run `0..extent` across
    /// the tile, `x` right and `y` down from its top-left corner. A node the
    /// render never reached — a branch a `switch` did not take — has no
    /// `extent` and no features. The values are the ones the render
    /// computed on its way to the pixels, so this costs no second
    /// evaluation.
    ///
    /// Throws `InvalidStyle` before anything renders when a name is not a
    /// node of the style, or names one that produces something other than
    /// features.
    #[wasm_bindgen(js_name = renderTileWithFeatures, unchecked_return_type = "TileWithFeatures")]
    pub fn render_tile_with_features(
        &self,
        z: u8,
        x: u32,
        y: u32,
        set: &SourceSet,
        nodes: Vec<String>,
        opts: Option<js_sys::Object>,
    ) -> Result<JsValue, JsValue> {
        let parsed = parse_render_options(opts.as_ref())?;
        let names: Vec<&str> = nodes.iter().map(String::as_str).collect();
        let (image, features) = self
            .inner
            .borrow()
            .render_tile_with_features(z, x, y, &set.inner, parsed, &names)
            .map_err(js_err)?;
        // The same shape as a page's features and as Go's: the text as the
        // renderer wrote it, for the host to parse or pass on as it is.
        let list = js_sys::Array::new();
        for f in &features {
            let entry = js_sys::Object::new();
            js_sys::Reflect::set(&entry, &"node".into(), &f.node().into())?;
            if let Some(value) = f.features() {
                js_sys::Reflect::set(&entry, &"extent".into(), &value.extent.into())?;
            }
            js_sys::Reflect::set(&entry, &"geojson".into(), &f.to_geojson().into())?;
            list.push(&entry);
        }
        let out = js_sys::Object::new();
        js_sys::Reflect::set(
            &out,
            &"image".into(),
            &js_sys::Uint8Array::from(image.as_slice()),
        )?;
        js_sys::Reflect::set(&out, &"features".into(), &list)?;
        Ok(out.into())
    }

    /// `neededCodepoints` for the features in `set` rather than the ones
    /// bound to the renderer: what to bind into the glyphs sources before
    /// rendering from it.
    #[wasm_bindgen(js_name = neededCodepointsIn)]
    pub fn needed_codepoints_in(&self, set: &SourceSet) -> Result<js_sys::Object, JsValue> {
        glyph_units_object(self.inner.borrow().needed_codepoints_in(&set.inner))
    }

    /// `neededGlyphRanges` for the features in `set` rather than the ones
    /// bound to the renderer.
    #[wasm_bindgen(js_name = neededGlyphRangesIn)]
    pub fn needed_glyph_ranges_in(&self, set: &SourceSet) -> Result<js_sys::Object, JsValue> {
        glyph_units_object(self.inner.borrow().needed_glyph_ranges_in(&set.inner))
    }

    /// Bring every bound fontstack back under the budget `setGlyphBudget`
    /// set; a no-op without one. `renderTile` does this after each render;
    /// after `renderTileWith`, call it yourself once a batch is done.
    /// Trimming can drop glyphs a frame bound for its next tile, so it
    /// throws `FrameInProgress` while a `renderFrame` holds the renderer,
    /// which trims after each of its tiles itself.
    #[wasm_bindgen(js_name = trimGlyphs)]
    pub fn trim_glyphs(&self) -> Result<(), JsValue> {
        self.idle()?;
        self.inner.borrow().trim_glyphs();
        Ok(())
    }
}

/// One tile-scoped payload — a vector tile, a DEM or imagery tile, a
/// GeoJSON document — decoded once by `Renderer.loadSource` and ready to
/// sit in any number of `SourceSet`s.
///
/// A set keeps what it holds alive, so `free()` once the sets that need it
/// have it, or leave it to the garbage collector. It is not tied to the
/// renderer that loaded it: renderers in one module share a heap, and a
/// render checks that its style declares the source the way the data was
/// loaded for.
#[wasm_bindgen]
pub struct SourceData {
    inner: ezu::renderer::SourceData,
}

#[wasm_bindgen]
impl SourceData {
    /// The `sources.<name>` entry the data was loaded for.
    #[wasm_bindgen(getter)]
    pub fn source(&self) -> String {
        self.inner.source().to_string()
    }
}

/// Every tile-scoped payload one render reads, each at its place in the
/// 3×3 neighbourhood: the binding table `bindSource` fills, as a value you
/// build per tile and hand to `renderTileWith`.
///
/// A set says nothing about the tile it is for — offsets are relative — so
/// pairing each tile with its neighbourhood is yours to do. Fonts, sprites,
/// brushes, images and glyphs are not here: they belong to the renderer,
/// bound once with `bindSource` and read by every render.
#[wasm_bindgen]
#[derive(Default)]
pub struct SourceSet {
    inner: ezu::renderer::SourceSet,
}

#[wasm_bindgen]
impl SourceSet {
    /// An empty set: a render from it draws what the style draws with no
    /// tile data at all.
    #[wasm_bindgen(constructor)]
    pub fn new() -> SourceSet {
        SourceSet::default()
    }

    /// Put `data` at `opts.coord` for its source, replacing what was
    /// there. `opts` is `bindSource`'s `{ coord?: [dx, dy], sourceZoom? }`:
    /// `sourceZoom` says the data is an ancestor's, to be resampled or
    /// reprojected into the tile. The set shares the data rather than
    /// copying it.
    ///
    /// Throws `UnknownSource` when the source already holds data of
    /// another kind, and for the `coord` and `sourceZoom` `bindSource`
    /// refuses.
    pub fn bind(&mut self, data: &SourceData, opts: Option<js_sys::Object>) -> Result<(), JsValue> {
        let parsed = parse_bind_options(opts.as_ref())?;
        self.inner
            .bind(&data.inner, &parsed)
            .map(|_| ())
            .map_err(js_err)
    }

    /// Whether anything is bound for `source`.
    pub fn contains(&self, source: &str) -> bool {
        self.inner.contains(source)
    }

    /// Whether nothing is bound at all.
    #[wasm_bindgen(js_name = isEmpty)]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// A copy of the set. It holds the same data, which is shared rather
    /// than copied, so this costs a few references per entry.
    #[wasm_bindgen(js_name = clone)]
    pub fn copy(&self) -> SourceSet {
        SourceSet {
            inner: self.inner.clone(),
        }
    }
}

/// What `frameFeatures` returns: features nodes' values gathered over the
/// tiles of a frame a host renders itself, assembled into the pages
/// `renderFrameWithFeatures` hands back.
///
/// The Rust renderer's collector is handed each tile's values after the
/// render; here the tile renders through the collector, so the values stay
/// in the module rather than crossing to JS and back. It keeps the
/// renderer that made it alive until it is finished or freed.
#[wasm_bindgen]
pub struct FrameFeatureCollector {
    renderer: Rc<RefCell<ezu::renderer::Renderer>>,
    inner: ezu::renderer::FrameFeatureCollector,
}

#[wasm_bindgen]
impl FrameFeatureCollector {
    /// `renderTileWith` for one of the frame's tiles, also keeping what the
    /// collector's nodes held. Pass `tileSize: frame.tileSize` in `opts` to
    /// draw the tile `renderFrame` draws. Like `renderTileWith` it works
    /// while a `renderFrame` holds the renderer, and leaves the glyph
    /// budget to `trimGlyphs()`.
    #[wasm_bindgen(js_name = renderTile)]
    pub fn render_tile(
        &mut self,
        z: u8,
        x: u32,
        y: u32,
        set: &SourceSet,
        opts: Option<js_sys::Object>,
    ) -> Result<Vec<u8>, JsValue> {
        let parsed = parse_render_options(opts.as_ref())?;
        let nodes: Vec<String> = self.inner.nodes().into_iter().map(String::from).collect();
        let names: Vec<&str> = nodes.iter().map(String::as_str).collect();
        let (image, values) = self
            .renderer
            .borrow()
            .render_tile_with_features(z, x, y, &set.inner, parsed, &names)
            .map_err(js_err)?;
        self.inner.add(ezu::renderer::TileId::new(z, x, y), &values);
        Ok(image)
    }

    /// The pages: one `{ node, geojson }` per entry of the request's
    /// `nodes`, in that order, as `renderFrameWithFeatures` hands them
    /// back. Tiles are read rows top to bottom, then left to right,
    /// whatever order they rendered in. The collector is used up.
    #[wasm_bindgen(unchecked_return_type = "PageFeatures[]")]
    pub fn finish(self) -> Result<js_sys::Array, JsValue> {
        page_list(self.inner)
    }
}

/// The pages `collector` assembled, as `{ node, geojson }` objects.
fn page_list(collector: ezu::renderer::FrameFeatureCollector) -> Result<js_sys::Array, JsValue> {
    let list = js_sys::Array::new();
    for page in collector.finish() {
        let entry = js_sys::Object::new();
        js_sys::Reflect::set(&entry, &"node".into(), &page.node().into())?;
        js_sys::Reflect::set(&entry, &"geojson".into(), &page.to_geojson().into())?;
        list.push(&entry);
    }
    Ok(list)
}

impl Renderer {
    /// `FrameInProgress` while a `renderFrame` holds the renderer.
    fn idle(&self) -> Result<(), JsValue> {
        if self.frame.busy.get() {
            return Err(shell_err(
                "FrameInProgress",
                "the renderer is rendering a frame; wait for renderFrame to settle, \
                 and bind what its tiles need through the binder it passes",
            ));
        }
        Ok(())
    }
}

/// What `renderFrame` hands each tile's `sources`: `bindSource` into the
/// renderer the frame has taken for itself, for what that tile's set needs
/// bound there.
#[wasm_bindgen]
pub struct FrameBinder {
    /// Weak, so a binder the host keeps does not keep the renderer alive.
    renderer: Weak<RefCell<ezu::renderer::Renderer>>,
    frame: Rc<FrameState>,
    tile: u64,
}

#[wasm_bindgen]
impl FrameBinder {
    /// `Renderer.bindSource`, for the sources bound into the renderer
    /// itself: the glyphs a tile turns out to need, or a font, sprite,
    /// brush or image. Throws `UnknownSource` for a tile-scoped source,
    /// which the frame renders from the returned set and would never read
    /// here, and `StaleBinder` once this tile's `sources` has returned (or
    /// its promise settled).
    #[wasm_bindgen(js_name = bindSource)]
    pub fn bind_source(
        &self,
        name: &str,
        bytes: Vec<u8>,
        opts: Option<js_sys::Object>,
    ) -> Result<(), JsValue> {
        let stale = || {
            shell_err(
                "StaleBinder",
                "this binder's tile is no longer being rendered; bind only from inside \
                 that tile's sources, before it returns or settles",
            )
        };
        if self.frame.open_tile.get() != self.tile {
            return Err(stale());
        }
        let renderer = self.renderer.upgrade().ok_or_else(stale)?;
        let tile_scoped = renderer
            .borrow()
            .sources()
            .iter()
            .any(|s| s.name == name && s.kind.is_tile_scoped());
        if tile_scoped {
            return Err(named_err(
                ErrorKind::UnknownSource,
                format!(
                    "source `{name}` is tile-scoped; load it and put it in the set \
                     the tile's sources returns"
                ),
            ));
        }
        bind_into(&renderer, name, bytes, opts.as_ref())
    }
}

fn bind_into(
    renderer: &RefCell<ezu::renderer::Renderer>,
    name: &str,
    bytes: Vec<u8>,
    opts: Option<&js_sys::Object>,
) -> Result<(), JsValue> {
    let parsed = parse_bind_options(opts)?;
    renderer
        .borrow_mut()
        .bind_source(name, bytes, &parsed)
        .map_err(js_err)
}

/// The tile loop of `renderFrame`, which owns the renderer while it runs.
/// With a collector, each tile also renders the nodes it gathers and hands
/// it their values.
async fn render_frame_tiles(
    inner: &Rc<RefCell<ezu::renderer::Renderer>>,
    state: &Rc<FrameState>,
    frame: &js_sys::Object,
    sources: Option<js_sys::Function>,
    opts: Option<js_sys::Object>,
    mut collector: Option<&mut ezu::renderer::FrameFeatureCollector>,
) -> Result<js_sys::Uint8Array, JsValue> {
    let frame = parse_frame(frame)?;
    let opts = frame_render_options(opts.as_ref(), frame.tile_size)?;
    // The page is assembled in JS memory, not in the wasm heap: the heap
    // never shrinks, and a 1929 × 1370 page is 10 MB that a 128 MB isolate
    // would otherwise carry for the rest of its life.
    let out = js_sys::Uint8Array::new_with_length(frame.width * frame.height * 4);
    let t = frame.tile_size;
    for y in frame.tiles_y.clone() {
        for x in frame.tiles_x.clone() {
            let mut set = ezu::renderer::SourceSet::new();
            if let Some(sources) = &sources {
                let tile = state.issued.get() + 1;
                state.issued.set(tile);
                state.open_tile.set(tile);
                let binder = FrameBinder {
                    renderer: Rc::downgrade(inner),
                    frame: Rc::clone(state),
                    tile,
                };
                // Resolving whatever came back awaits a promise and lets a
                // plain return through after one microtask.
                let ret = sources.call2(
                    &JsValue::NULL,
                    &tile_object(frame.zoom, x, y)?,
                    &binder.into(),
                )?;
                let settled = JsFuture::from(js_sys::Promise::resolve(&ret)).await;
                state.open_tile.set(0);
                set = returned_set(&settled?)?;
            }
            let px = match collector.as_deref_mut() {
                None => inner
                    .borrow()
                    .render_tile_with(frame.zoom, x, y, &set, opts.clone()),
                Some(c) => inner
                    .borrow()
                    .render_tile_with_features(frame.zoom, x, y, &set, opts.clone(), &c.nodes())
                    .map(|(px, values)| {
                        c.add(ezu::renderer::TileId::new(frame.zoom, x, y), &values);
                        px
                    }),
            };
            inner.borrow().trim_glyphs();
            let px = px.map_err(js_err)?;
            let want = t as usize * t as usize * 4;
            if px.len() != want {
                return Err(named_err(
                    ErrorKind::RenderFailed,
                    format!(
                        "tile {}/{x}/{y} came back as {} bytes, want {want} for {t}x{t}",
                        frame.zoom,
                        px.len()
                    ),
                ));
            }
            for row in frame.tile_rows(x, y) {
                out.subarray(row.dst as u32, (row.dst + row.len) as u32)
                    .copy_from(&px[row.src..row.src + row.len]);
            }
        }
    }
    Ok(out)
}

/// The set a frame's `sources` returned, read without taking it from the
/// host.
///
/// wasm-bindgen turns a JS object back into its Rust struct only by moving
/// it out, which would leave the host holding a freed set — and a host may
/// well return one set for several tiles. So the frame asks the set for a
/// copy through its own `clone()`, which shares the data, and takes that.
fn returned_set(value: &JsValue) -> Result<ezu::renderer::SourceSet, JsValue> {
    use wasm_bindgen::convert::TryFromJsValue;
    let not_a_set = || -> JsValue {
        js_sys::TypeError::new("a frame's sources must return a SourceSet (or a promise of one)")
            .into()
    };
    if value.is_undefined() || value.is_null() {
        return Err(not_a_set());
    }
    let copy = js_sys::Reflect::get(value, &"clone".into())
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .ok_or_else(not_a_set)?
        .call0(value)?;
    SourceSet::try_from_js_value(copy)
        .map(|set| set.inner)
        .map_err(|_| not_a_set())
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

#[wasm_bindgen(typescript_custom_section)]
const FEATURE_TYPES: &'static str = r#"
/** What one features node held when a render finished. */
export interface NodeFeatures {
  /** The node's id, as it was asked for. */
  node: string;
  /** The tile's size in geometry units: coordinates run 0..extent, x right and y down. Absent when the render never reached the node. */
  extent?: number;
  /** A GeoJSON FeatureCollection as text, in those units, with `node` and `extent` members of its own. */
  geojson: string;
}

/** What `renderTileWithFeatures` hands back. */
export interface TileWithFeatures {
  /** The encoded tile, exactly as `renderTileWith` returns it. */
  image: Uint8Array;
  /** One collection per node asked for, in the order asked. */
  features: NodeFeatures[];
}

/** Which features nodes `renderFrameWithFeatures` gathers over a page, and how it writes them. */
export interface FrameFeatures {
  /** One page of features per entry, in this order. `key` names the properties that identify a feature across tiles; with one, each feature is written once and the page is sorted by it. */
  nodes: Array<{ node: string; key?: string[] }>;
  /** WGS84 longitude and latitude (the default), or page pixels. */
  coords?: "lonlat" | "px";
}

/** What one features node held over a whole page. */
export interface PageFeatures {
  /** The node's id, as it was asked for. */
  node: string;
  /** A GeoJSON FeatureCollection as text: byte for byte the file `ezu frame --features` writes. */
  geojson: string;
}

/** What `renderFrameWithFeatures` hands back. */
export interface FrameWithFeatures {
  /** The page, exactly as `renderFrame` resolves to it. */
  image: Uint8Array;
  /** One page of features per node asked for, in the order asked. */
  features: PageFeatures[];
}
"#;

#[wasm_bindgen(typescript_custom_section)]
const FRAME_TYPES: &'static str = r#"
/** What `computeFrame` is asked for: a print page as scale, dpi, size and centre. */
export interface FrameRequest {
  /** The frame's centre in degrees (WGS84). The scale is matched at this latitude. */
  centerLon: number;
  centerLat: number;
  /** The scale denominator: 1000 for 1:1000. */
  scale: number;
  /** The output resolution. Together with `scale` it fixes the ground size of a pixel. */
  dpi: number;
  /** The output size in pixels: `Math.round(mm / 25.4 * dpi)` from paper millimetres. */
  width: number;
  height: number;
  /** Render at this zoom, deriving the tile size from it, as the CLI's `--zoom`. */
  zoom?: number;
  /** The tile size the chosen zoom aims at. Defaults to this renderer's style. */
  styleTileSize?: number;
}

/** Where a print frame sits on the tile grid it renders from. */
export interface Frame {
  /** The zoom the tiles render at. */
  zoom: number;
  /** The tile size, in whole pixels, the tiles render at. */
  tileSize: number;
  /** The scale denominator this zoom and tile size actually meet at the centre. */
  scale: number;
  /** The output size in pixels, as requested. */
  width: number;
  height: number;
  /** The frame's top-left corner in world pixels at this zoom and tile size. */
  left: number;
  top: number;
  /** How far rounding onto whole pixels moved the frame: the centre sits at (width/2 + shift[0], height/2 + shift[1]). */
  shift: [number, number];
  /** The tile columns and rows the frame touches, start inclusive, end exclusive. */
  tilesX: [number, number];
  tilesY: [number, number];
}
"#;

/// Read `req` as a [`FrameRequest`], with `styleTileSize` defaulting to
/// the renderer's style. The same fields, with the same meaning, as the C
/// ABI's `ezu_compute_frame` takes.
fn parse_frame_request(
    req: &js_sys::Object,
    style_tile_size: u32,
) -> Result<FrameRequest, JsValue> {
    let required = |key: &str| -> Result<f64, JsValue> {
        number_field(req, key)?.ok_or_else(|| {
            named_err(
                ErrorKind::InvalidStyle,
                format!("the frame request needs {key} as a number"),
            )
        })
    };
    let zoom = match number_field(req, "zoom")? {
        None => None,
        Some(z) if z.fract() == 0.0 && (0.0..=31.0).contains(&z) => Some(z as u8),
        Some(z) => {
            return Err(named_err(
                ErrorKind::InvalidStyle,
                format!("zoom must be a whole zoom level in 0..=31, got {z}"),
            ))
        }
    };
    let style_tile_size = match number_field(req, "styleTileSize")? {
        Some(n) => whole_pixels(n, "styleTileSize")?,
        None => style_tile_size,
    };
    Ok(FrameRequest {
        center_lon: required("centerLon")?,
        center_lat: required("centerLat")?,
        scale: required("scale")?,
        dpi: required("dpi")?,
        width: whole_pixels(required("width")?, "width")?,
        height: whole_pixels(required("height")?, "height")?,
        zoom,
        style_tile_size,
    })
}

/// `frame` as the object `computeFrame` answers with.
fn frame_object(frame: &Frame) -> Result<JsValue, JsValue> {
    let out = js_sys::Object::new();
    let set = |key: &str, value: JsValue| js_sys::Reflect::set(&out, &key.into(), &value);
    let pair = |a: f64, b: f64| -> JsValue {
        let arr = js_sys::Array::new();
        arr.push(&JsValue::from_f64(a));
        arr.push(&JsValue::from_f64(b));
        arr.into()
    };
    set("zoom", JsValue::from(frame.zoom))?;
    set("tileSize", JsValue::from(frame.tile_size))?;
    set("scale", JsValue::from_f64(frame.scale))?;
    set("width", JsValue::from(frame.width))?;
    set("height", JsValue::from(frame.height))?;
    // World pixels stay well inside a double's integers: 8192 px tiles at
    // zoom 31 are 2^44 px across.
    set("left", JsValue::from_f64(frame.left as f64))?;
    set("top", JsValue::from_f64(frame.top as f64))?;
    set("shift", pair(frame.shift.0, frame.shift.1))?;
    let (tx, ty) = (&frame.tiles_x, &frame.tiles_y);
    set("tilesX", pair(tx.start as f64, tx.end as f64))?;
    set("tilesY", pair(ty.start as f64, ty.end as f64))?;
    Ok(out.into())
}

/// Read back a frame `computeFrame` answered with — the fields
/// `renderFrame` places tiles by. `scale` and `shift` only describe the
/// frame, so they are not read.
fn parse_frame(obj: &js_sys::Object) -> Result<Frame, JsValue> {
    let required = |key: &str| -> Result<f64, JsValue> {
        number_field(obj, key)?.ok_or_else(|| {
            named_err(
                ErrorKind::InvalidStyle,
                format!("the frame has no {key}; compute it with computeFrame"),
            )
        })
    };
    let whole = |key: &str| -> Result<i64, JsValue> {
        let n = required(key)?;
        if n.fract() != 0.0 || n.abs() > 2f64.powi(53) {
            return Err(named_err(
                ErrorKind::InvalidStyle,
                format!("the frame's {key} must be a whole number of pixels, got {n}"),
            ));
        }
        Ok(n as i64)
    };
    let range = |key: &str| -> Result<std::ops::Range<u32>, JsValue> {
        let bad = || {
            named_err(
                ErrorKind::InvalidStyle,
                format!("the frame's {key} must be a [start, end] pair of tile numbers"),
            )
        };
        let arr = js_sys::Reflect::get(obj, &key.into())?
            .dyn_into::<js_sys::Array>()
            .map_err(|_| bad())?;
        if arr.length() != 2 {
            return Err(bad());
        }
        let end = |v: JsValue| {
            v.as_f64()
                .ok_or_else(bad)
                .and_then(|n| whole_pixels(n, key))
        };
        Ok(end(arr.get(0))?..end(arr.get(1))?)
    };
    let zoom = required("zoom")?;
    if zoom.fract() != 0.0 || !(0.0..=31.0).contains(&zoom) {
        return Err(named_err(
            ErrorKind::InvalidStyle,
            format!("the frame's zoom must be a whole zoom level in 0..=31, got {zoom}"),
        ));
    }
    let frame = Frame {
        zoom: zoom as u8,
        tile_size: whole_pixels(required("tileSize")?, "tileSize")?,
        scale: 0.0,
        width: whole_pixels(required("width")?, "width")?,
        height: whole_pixels(required("height")?, "height")?,
        left: whole("left")?,
        top: whole("top")?,
        shift: (0.0, 0.0),
        tiles_x: range("tilesX")?,
        tiles_y: range("tilesY")?,
    };
    if frame.tile_size == 0 || frame.width == 0 || frame.height == 0 {
        return Err(named_err(
            ErrorKind::InvalidStyle,
            "the frame has no size; compute it with computeFrame",
        ));
    }
    Ok(frame)
}

/// `renderTile`'s options for a frame's tiles: the caller's `pad` and
/// `params`, at the frame's tile size, as raw pixels. A `format` or
/// `tileSize` that contradicts the frame is refused rather than
/// overridden, since either would mean the caller expects something else.
fn frame_render_options(
    obj: Option<&js_sys::Object>,
    tile_size: u32,
) -> Result<RenderOptions, JsValue> {
    if let Some(obj) = obj {
        let format = js_sys::Reflect::get(obj, &"format".into())?;
        if let Some(f) = format.as_string().filter(|f| f != "rgba") {
            return Err(named_err(
                ErrorKind::InvalidStyle,
                format!("a frame renders as raw pixels; leave format out, not \"{f}\""),
            ));
        }
        if let Some(n) = number_field(obj, "tileSize")?.filter(|n| *n != tile_size as f64) {
            return Err(named_err(
                ErrorKind::InvalidStyle,
                format!("the frame renders at tile size {tile_size}; leave tileSize out, not {n}"),
            ));
        }
    }
    let mut opts = parse_render_options(obj)?;
    opts.format = OutputFormat::Rgba;
    opts.tile_size = Some(tile_size);
    Ok(opts)
}

/// `{ z, x, y }` for the tile a frame is about to render.
fn tile_object(z: u8, x: u32, y: u32) -> Result<JsValue, JsValue> {
    let out = js_sys::Object::new();
    js_sys::Reflect::set(&out, &"z".into(), &JsValue::from(z))?;
    js_sys::Reflect::set(&out, &"x".into(), &JsValue::from(x))?;
    js_sys::Reflect::set(&out, &"y".into(), &JsValue::from(y))?;
    Ok(out.into())
}

/// Read `renderFrameWithFeatures`'s `{ nodes: [{ node, key? }], coords? }`,
/// with the names and refusals the C ABI's `ezu_frame_features_new` has.
fn parse_frame_features(obj: &js_sys::Object) -> Result<FrameFeatures, JsValue> {
    let bad_nodes = || {
        named_err(
            ErrorKind::InvalidStyle,
            "the frame features request needs nodes as an array of { node: id, key?: [property, …] }",
        )
    };
    let nodes = js_sys::Reflect::get(obj, &"nodes".into())?
        .dyn_into::<js_sys::Array>()
        .map_err(|_| bad_nodes())?
        .iter()
        .map(|entry| {
            let node = js_sys::Reflect::get(&entry, &"node".into())?
                .as_string()
                .ok_or_else(bad_nodes)?;
            let key = js_sys::Reflect::get(&entry, &"key".into())?;
            let key = if key.is_undefined() || key.is_null() {
                None
            } else {
                let props = key.dyn_into::<js_sys::Array>().map_err(|_| bad_nodes())?;
                Some(
                    props
                        .iter()
                        .map(|p| p.as_string().ok_or_else(bad_nodes))
                        .collect::<Result<Vec<_>, _>>()?,
                )
            };
            Ok(FrameFeatureNode { node, key })
        })
        .collect::<Result<Vec<_>, JsValue>>()?;
    let coords = js_sys::Reflect::get(obj, &"coords".into())?;
    let coords = if coords.is_undefined() || coords.is_null() {
        FeatureCoords::default()
    } else {
        coords
            .as_string()
            .as_deref()
            .and_then(FeatureCoords::from_name)
            .ok_or_else(|| {
                named_err(
                    ErrorKind::InvalidStyle,
                    "coords must be \"lonlat\" or \"px\"",
                )
            })?
    };
    Ok(FrameFeatures { nodes, coords })
}

/// `obj[key]` as a number, `None` when it is absent (`undefined` or
/// `null`), and `InvalidStyle` when it is something else.
fn number_field(obj: &js_sys::Object, key: &str) -> Result<Option<f64>, JsValue> {
    let value = js_sys::Reflect::get(obj, &key.into())?;
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    value
        .as_f64()
        .map(Some)
        .ok_or_else(|| named_err(ErrorKind::InvalidStyle, format!("{key} must be a number")))
}

/// `n` as a whole, non-negative count of pixels (or tiles).
fn whole_pixels(n: f64, label: &str) -> Result<u32, JsValue> {
    if n.fract() != 0.0 || !(0.0..=u32::MAX as f64).contains(&n) {
        return Err(named_err(
            ErrorKind::InvalidStyle,
            format!("{label} must be a whole number of pixels, got {n}"),
        ));
    }
    Ok(n as u32)
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
/// `memory.grow` throws `OutOfMemory` (see the `oom` module) and ends the
/// instance.
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
    shell_err(kind.name(), e)
}

/// A JS `Error` named `name`, for the failures only this shell has —
/// `FrameInProgress` and `StaleBinder`, which are about JS calling into a renderer a
/// frame holds and have no counterpart in [`ErrorKind`].
fn shell_err(name: &str, e: impl std::fmt::Display) -> JsValue {
    let err = js_sys::Error::new(&e.to_string());
    err.set_name(name);
    err.into()
}
