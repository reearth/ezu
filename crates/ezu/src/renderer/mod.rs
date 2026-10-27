//! The embedding renderer: the host fetches, binds each source's bytes,
//! and renders a tile or a print frame.
//!
//! An embedding is ezu running somewhere that owns its own I/O: a Rust
//! service with its own HTTP client and tile cache, a browser tab, a Go
//! process driving a wasm runtime. A Rust host uses this module directly.
//! The JS package (`ezu-wasm`) and the Go package (through `ezu-cabi`)
//! are shells over it that speak their host's dialect — JS objects, JSON
//! strings, C pointers — so the three offer the same calls, render the
//! same bytes, and name the same failure the same way.
//!
//! [`Renderer`] holds a parsed style document, its built graph, an
//! in-memory asset bank, and a per-tile binding buffer that mirrors the
//! style's `sources` block. The flow mirrors the CLI's: bind every
//! source the style declares, render, clear. [`Renderer::sources`] says
//! what those are and where to fetch them, so a host drives the loop
//! without reading the style itself.
//!
//! ```no_run
//! # use ezu::renderer::{BindOptions, RenderOptions, Renderer};
//! # fn fetch_mvt(z: u8, x: u32, y: u32) -> Vec<u8> { unimplemented!() }
//! # fn main() -> Result<(), ezu::renderer::Error> {
//! # let style_json = "";
//! let mut r = Renderer::new(style_json)?;
//! r.bind_source("basemap", fetch_mvt(12, 1, 2), &BindOptions::default())?;
//! let png = r.render_tile(12, 1, 2, RenderOptions::default())?;
//! r.clear_sources();
//! # Ok(())
//! # }
//! ```
//!
//! The renderer dispatches on each source's declared `type`:
//! - `brush` → parse `.myb` JSON into the persistent brush bank under
//!   the source's `src` (unaffected by [`Renderer::clear_sources`])
//! - `image` → decode PNG/WebP into the persistent image bank (same
//!   persistence)
//! - `sprite` → decode the atlas image plus its index (inline in the
//!   style, or [`BindOptions::index`]) into the persistent sprite bank
//! - `font` → parse TTF/OTF/TTC bytes into the persistent font bank
//! - `glyphs` → decode one SDF glyph PBF per call into the persistent
//!   glyph bank; glyphs are filed by id, so repeated calls accumulate
//!   and a payload may be a whole `{range}.pbf` or any subset
//! - `mvt` / `pmtiles` → MVT decode, bound as `<source>.<layer>` at
//!   render time (cleared by `clear_sources`)
//! - `dem` / `raster` → decode and 3×3 stitch at render time, bound
//!   under the bare source name (cleared by `clear_sources`)
//! - `geojson` → *remote* GeoJSON only, projected per tile at render
//!   time (cleared by `clear_sources`). Inline `data` needs no binding.
//!
//! The binding table is state a host has to reset between tiles, and it
//! decodes each payload every time it is bound. The tile-scoped kinds can
//! instead be loaded once and handed to each render explicitly: a
//! [`SourceData`] from [`Renderer::load_source`] is a decoded payload, a
//! [`SourceSet`] is what one render reads, and
//! [`Renderer::render_tile_with`] renders from a set without touching the
//! table — so one tile's data serves as its neighbours' neighbour too, and
//! renders on several threads can share one `&Renderer`.
//!
//! ```no_run
//! # use ezu::renderer::{BindOptions, RenderOptions, Renderer, SourceSet};
//! # fn fetch_mvt(z: u8, x: u32, y: u32) -> Vec<u8> { unimplemented!() }
//! # fn main() -> Result<(), ezu::renderer::Error> {
//! # let style_json = "";
//! let r = Renderer::new(style_json)?;
//! let data = r.load_source("basemap", &fetch_mvt(12, 1, 2))?;
//! let mut set = SourceSet::new();
//! set.bind(&data, &BindOptions::default())?;
//! let png = r.render_tile_with(12, 1, 2, &set, RenderOptions::default())?;
//! # Ok(())
//! # }
//! ```
//!
//! The renderer fetches nothing itself, so everything a render needs
//! must be bound before it: [`Renderer::requested_neighbor_offsets`]
//! names the neighbour tiles, and [`Renderer::needed_codepoints`] the
//! glyphs. A native host built with the `http` feature can instead let the
//! renderer read the style's brushes, images, sprites, fonts and glyphs
//! itself with `Renderer::resolve_assets`, glyph ranges as labels need
//! them, which is how the `ezu` CLI renders; tile data stays the host's to
//! fetch either way.
//!
//! Every fallible call returns an [`Error`] carrying an [`ErrorKind`]
//! whose [`name`](ErrorKind::name) is what a host branches on.

#[cfg(feature = "http")]
mod assets;
mod error;
mod features;
mod frame;
mod frame_features;
mod options;
mod source_set;
mod sources;

pub use error::{Error, ErrorKind};
pub use features::{FeatureGroup, FilteredFeatures, NodeFeatures, TileFeatureCoords};
pub use frame::{FrameBinder, RenderFrameError};
pub use frame_features::{
    FeatureCoords, FrameFeatureCollector, FrameFeatureNode, FrameFeatures, PageFeatures,
};
pub use options::{BindOptions, OutputFormat, RenderOptions, MAX_PIXEL_RATIO};
pub use source_set::{SourceData, SourceSet};
pub use sources::{SourceInfo, SourceKind};

use source_set::Payload;

/// Print-frame geometry, re-exported so a shell needs this crate and
/// nothing under it. See [`Renderer::compute_frame`] and
/// [`Renderer::render_frame`].
pub use ezu_core::frame::{Frame, FrameRequest, RowCopy};

/// A Web Mercator tile, as [`Renderer::render_frame`] names the tile it
/// is about to render. Re-exported so a shell needs this crate and nothing
/// under it.
pub use ezu_core::TileId;

/// Effort the PNG encoder spends, named by
/// [`RenderOptions::png_compression`]. Re-exported so a shell needs this
/// crate and nothing under it.
pub use ezu_paint::host::PngCompression;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use ezu_core::TileId as CoreTileId;
use ezu_graph::{
    build_graph, parse_param_value, Cache, CanvasInfo, Evaluator, Graph, ParamValues, PortValue,
    SpriteSheet, TileId as GraphTileId,
};
use ezu_paint::host::{
    build_sprite_icons, crop_to_png, crop_to_rgba8, crop_to_webp, decode_dem_tile,
    decode_raster_tile, stitch_padded_field, stitch_padded_raster, upsample_subregion,
    upsample_subregion_raster, BrushBankLoader, DemTile, RasterTile, TileLoader,
};
use ezu_paint::nodes::default_registry;
use ezu_style::{Document, OnMissing, SourceDecl};

use ErrorKind::*;

fn err(kind: ErrorKind, e: impl std::fmt::Display) -> Error {
    Error::new(kind, e)
}

/// Payload sizes of what a renderer is holding, so a host can shed load
/// *before* an allocation fails rather than after.
///
/// These are payload sizes, not an accounting of the heap: they omit
/// allocator overhead, decoded features, per-font glyph-path caches, and
/// the buffers a render is using right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryUsage {
    /// SDF bitmaps resident in the glyph bank, across every fontstack.
    pub glyph_bytes: usize,
    /// How many 256-codepoint blocks those bitmaps span.
    pub glyph_ranges: usize,
    /// The per-fontstack ceiling [`Renderer::set_glyph_budget`] put on
    /// them, or `None` when none is set. `glyph_bytes` totals *every*
    /// fontstack, so it can exceed the budget legitimately.
    pub glyph_budget: Option<usize>,
    /// Outline font files held in the font bank.
    pub font_bytes: usize,
    /// Decoded pixels of bound images and sprite atlases.
    pub image_bytes: usize,
    /// The render cache's pixel payload.
    pub cache_bytes: usize,
    /// The cache's own eviction budget. It bounds itself, so
    /// `cache_bytes` near `cache_budget` is steady state, not a leak.
    pub cache_budget: usize,
}

/// A style, its built graph, its asset banks, and whatever tiles are
/// currently bound to it.
pub struct Renderer {
    doc: Document,
    graph: Arc<Graph>,
    cache: Arc<Cache>,
    assets: BrushBankLoader,
    /// What `bind_source` has bound for the next `render_tile`: the set
    /// `render_tile_with` would be handed, kept here for hosts that bind
    /// and clear. Cleared by [`Renderer::clear_sources`].
    bindings: SourceSet,
    /// Ceiling on resident glyph bytes per fontstack, applied after each
    /// render. `None` (the default) keeps every bound range.
    glyph_budget: Option<usize>,
    /// Tile size every render uses in place of the style's `tile-size`,
    /// set by [`Renderer::set_tile_size`]. `None` renders at the style's.
    tile_size_override: Option<u32>,
    /// Pixel bytes the render cache may hold, set by
    /// [`Renderer::set_cache_budget`]. `None` keeps the graph cache's own
    /// default.
    cache_budget: Option<usize>,
    /// Held shared by every render while it runs, and exclusively by a
    /// glyph trim, so a trim never drops glyphs a render on another thread
    /// was bound with.
    glyph_gate: RwLock<()>,
    /// A trim that met a render in flight, owed by the last render to
    /// finish.
    trim_pending: AtomicBool,
}

impl Renderer {
    /// Build a renderer from a style JSON document.
    pub fn new(style_json: &str) -> Result<Self, Error> {
        let (doc, graph) = parse_and_build(style_json)?;
        Ok(Self {
            doc,
            graph: Arc::new(graph),
            cache: new_cache(None),
            assets: BrushBankLoader::new(),
            bindings: SourceSet::new(),
            glyph_budget: None,
            tile_size_override: None,
            cache_budget: None,
            glyph_gate: RwLock::new(()),
            trim_pending: AtomicBool::new(false),
        })
    }

    /// Let the render cache hold up to `bytes` of pixels instead of its
    /// default (8 MiB). `None` goes back to the default.
    ///
    /// The cache keeps intermediate results a later render can reuse — most
    /// of all the ones a world-anchored node computes, which every tile
    /// around it reads too — so a host rendering many neighbouring tiles
    /// with memory to spare renders them faster with more of it, and a host
    /// short of memory can hold it to less. It bounds itself:
    /// [`MemoryUsage::cache_bytes`] near the budget is steady state.
    ///
    /// The cache is dropped and starts empty at the new size. The budget
    /// stays through [`set_style`](Self::set_style) and
    /// [`set_tile_size`](Self::set_tile_size), which start a fresh cache of
    /// the same size.
    pub fn set_cache_budget(&mut self, bytes: Option<usize>) {
        self.cache_budget = bytes;
        self.cache = new_cache(bytes);
    }

    /// Replace the active style. Returns the new node count. Invalidates
    /// the intermediate cache, drops any pending source bindings, and
    /// clears a tile size set with [`set_tile_size`](Self::set_tile_size):
    /// such a size is derived from the style's own (a print frame's is),
    /// so it does not carry over to another style.
    pub fn set_style(&mut self, style_json: &str) -> Result<usize, Error> {
        let (doc, graph) = parse_and_build(style_json)?;
        let n = doc.nodes.len();
        self.doc = doc;
        self.graph = Arc::new(graph);
        self.cache = new_cache(self.cache_budget);
        self.bindings.clear();
        self.tile_size_override = None;
        Ok(n)
    }

    /// `tile-size` declared by the current style, whatever
    /// [`set_tile_size`](Self::set_tile_size) renders at.
    pub fn tile_size(&self) -> u32 {
        self.doc.tile_size
    }

    /// The tile size renders use: the one
    /// [`set_tile_size`](Self::set_tile_size) set, or else the style's.
    pub fn render_tile_size(&self) -> u32 {
        self.tile_size_override.unwrap_or(self.doc.tile_size)
    }

    /// Render every tile at `size` px instead of the style's `tile-size`,
    /// without editing the style. `None` goes back to the style's.
    ///
    /// This is how a print frame renders: [`Frame::compute`] meets a map
    /// scale by choosing the tile size, and the tiles must then be drawn at
    /// exactly that size. It renders what a copy of the style with its
    /// `tile-size` changed would. Nothing else read from the style depends
    /// on the tile size: the graph, the pad, the neighbour offsets and the
    /// source tiles a host binds stay as they are, and the canvas — through
    /// it raster and DEM stitching, GeoJSON projection and label placement —
    /// follows the new size.
    ///
    /// The intermediate cache is sized by the tile, so it is dropped when
    /// the size actually changes; the asset banks and pending bindings are
    /// kept. A [`RenderOptions::tile_size`] on one render still wins over
    /// this.
    ///
    /// Fails with [`ErrorKind::InvalidStyle`] for a size of zero.
    pub fn set_tile_size(&mut self, size: Option<u32>) -> Result<(), Error> {
        if size == Some(0) {
            return Err(err(InvalidStyle, "tile size must be at least 1 px"));
        }
        let before = self.render_tile_size();
        self.tile_size_override = size;
        if self.render_tile_size() != before {
            self.cache = new_cache(self.cache_budget);
        }
        Ok(())
    }

    /// Where a print frame sits on the tile grid: the zoom and tile size
    /// that meet `req`'s scale, and the tiles that cover it. See
    /// [`ezu_core::frame`].
    ///
    /// `req.style_tile_size` is what the default zoom aims the tile size
    /// at; pass [`tile_size`](Self::tile_size) for this renderer's style,
    /// which is what `ezu frame` does. Render the result with
    /// [`set_tile_size`](Self::set_tile_size) set to [`Frame::tile_size`].
    ///
    /// Fails with [`ErrorKind::InvalidStyle`] when the request is out of
    /// its domain or the scale cannot be met at a usable tile size.
    pub fn compute_frame(&self, req: &FrameRequest) -> Result<Frame, Error> {
        Frame::compute(req).map_err(|e| err(InvalidStyle, e))
    }

    /// What building the current style's graph warned about: each names a
    /// node that works but relies on something the style does not say out
    /// loud. They are also logged when the style is built, as `tracing`
    /// warnings; this is for a host that reports or checks them itself —
    /// failing a build on any, say.
    pub fn warnings(&self) -> &[String] {
        self.graph.warnings()
    }

    /// The current style's built graph: the one every render evaluates.
    ///
    /// Only in Rust. The graph is the evaluator's in-memory structure, with
    /// a node object per op, and has no form across the wasm boundary the
    /// JS and Go packages sit behind; what a host there needs from it is
    /// answered by calls of its own — [`warnings`](Self::warnings),
    /// [`requested_neighbor_offsets`](Self::requested_neighbor_offsets),
    /// [`render_tile_with_features`](Self::render_tile_with_features). A
    /// Rust host can read it instead of building the style a second time:
    /// the canvas margin it needs, the node indices
    /// [`render_tile_observed`](Self::render_tile_observed) reports.
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// JSON Schema for the current style's `params` — types, defaults,
    /// ranges, descriptions.
    pub fn params_schema(&self) -> serde_json::Value {
        self.doc.params_schema()
    }

    /// Effective attribution declared by the style (document + sources),
    /// joined with ` | `. Upstream TileJSON / PMTiles metadata is the
    /// host's concern — merge it on that side.
    pub fn attribution(&self) -> Option<String> {
        let list = self.doc.attributions();
        if list.is_empty() {
            None
        } else {
            Some(list.join(" | "))
        }
    }

    /// The style's declared `legend` as JSON text, or `None` when it
    /// declares none. Pass a zoom to keep only the entries that apply
    /// there.
    ///
    /// Text rather than a [`serde_json::Value`] because the declaration
    /// order of a legend's fields is part of what a host renders, and a
    /// `Value` would sort them.
    pub fn legend_json(&self, zoom: Option<u8>) -> Result<Option<String>, Error> {
        let Some(legend) = &self.doc.legend else {
            return Ok(None);
        };
        let filtered = zoom.map(|z| ezu_style::LegendDecl {
            title: legend.title.clone(),
            note: legend.note.clone(),
            entries: legend.entries_at(z).cloned().collect(),
        });
        serde_json::to_string(filtered.as_ref().unwrap_or(legend))
            .map(Some)
            .map_err(|e| err(InvalidStyle, e))
    }

    /// Drop every pending source binding. Call between tile renders.
    pub fn clear_sources(&mut self) {
        self.bindings.clear();
    }

    /// Every source the style declares, in declaration order: its name,
    /// what kind it is, and where its bytes come from.
    ///
    /// This is what a host walks before it fetches anything.
    /// [`bound_sources`](Self::bound_sources) answers the opposite
    /// question — what is already bound — and cannot start the loop. The
    /// alternative to this call is for each host to parse the style a
    /// second time and re-derive the dispatch
    /// [`bind_source`](Self::bind_source) already performs, which is the
    /// style interpretation the renderer exists to keep in one place.
    ///
    /// [`SourceKind::is_tile_scoped`] says whether a binding is dropped by
    /// [`clear_sources`](Self::clear_sources) and so has to be rebound per
    /// tile; [`SourceInfo::url`] is `None` only for an inline `geojson`,
    /// which needs no binding at all.
    pub fn sources(&self) -> Vec<SourceInfo<'_>> {
        self.doc
            .sources
            .iter()
            .map(|(name, decl)| SourceInfo::new(name, decl))
            .collect()
    }

    /// Names of every source with at least one pending binding. Order
    /// matches the style's `sources` declaration order.
    pub fn bound_sources(&self) -> Vec<String> {
        self.doc
            .sources
            .keys()
            .filter(|n| self.bindings.contains(n))
            .cloned()
            .collect()
    }

    /// Cap the glyph bytes each bound fontstack keeps resident, in bytes.
    /// `None` lifts the cap, which is where a fresh renderer starts, and
    /// is the spelling [`MemoryUsage::glyph_budget`] reports it back with.
    ///
    /// Uncapped, a fontstack keeps every range ever bound to it for the
    /// life of the renderer — `clear_sources` does not touch glyphs, and
    /// on a long-lived instance rendering across a basemap that is usually
    /// what grew. Trimming happens **after** each render, not while
    /// binding, so a render never loses glyphs that were bound for it.
    ///
    /// It is a per-fontstack ceiling: a style with a regular, a medium
    /// and an italic stack can hold three times what is set here.
    pub fn set_glyph_budget(&mut self, bytes: Option<usize>) {
        self.glyph_budget = bytes;
        for stack in self
            .assets
            .glyphs
            .read()
            .expect("glyphs bank poisoned")
            .values()
        {
            stack.set_byte_budget(bytes.unwrap_or(usize::MAX));
        }
    }

    /// Bind raw bytes under a `sources.<name>` entry from the style,
    /// dispatching on the source's declared type (see the crate docs).
    ///
    /// Fails with [`ErrorKind::UnknownSource`] if `name` is not declared,
    /// or if it is already bound as a different kind; with the matching
    /// decode kind if the payload does not parse.
    pub fn bind_source(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
        opts: &BindOptions,
    ) -> Result<(), Error> {
        let decl = self
            .doc
            .sources
            .get(name)
            .ok_or_else(|| err(UnknownSource, format!("no source `{name}` in style")))?;
        match decl {
            SourceDecl::Brush(file) => {
                // Brushes are document-scoped: register into the
                // persistent BrushBankLoader keyed by `decl.src`
                // (which is what the `brush-file` node looks up).
                let src_key = file.src.clone();
                let bytes_str = std::str::from_utf8(&bytes).map_err(|e| err(BrushParse, e))?;
                let brush = hokusai::myb::from_str(bytes_str).map_err(|e| err(BrushParse, e))?;
                self.assets.insert(src_key, brush);
            }
            SourceDecl::Image(file) => {
                // Same persistent lifetime as brushes — keyed by
                // `decl.src` so the `image` node finds it.
                let src_key = file.src.clone();
                let raster = ezu_paint::host::decode_image_bytes(&bytes)
                    .map_err(|e| err(MvtDecode, format!("image decode: {e}")))?;
                self.assets.insert_image(src_key, raster);
            }
            // Vector tiles and remote GeoJSON decode here, once, and the
            // render binds what was decoded. (Inline GeoJSON `data` needs
            // no bind — it's read from the document at render time.)
            SourceDecl::Mvt(_) | SourceDecl::Pmtiles(_) | SourceDecl::GeoJson(_) => {
                let data = self.load_source(name, &bytes)?;
                self.bindings.bind(&data, opts)?;
            }
            // DEM and imagery bytes stay raw until the render, as they
            // always have here: their decode errors surface from
            // `render_tile` and name the tile, which callers rely on.
            // `load_source` is the way to decode them up front.
            SourceDecl::Dem(_) | SourceDecl::Raster(_) => {
                let raw: Arc<[u8]> = bytes.into();
                let payload = if matches!(decl, SourceDecl::Dem(_)) {
                    Payload::DemBytes(raw.clone())
                } else {
                    Payload::RasterBytes(raw.clone())
                };
                let data = SourceData::new(name, payload, &raw);
                self.bindings.bind(&data, opts)?;
            }
            // Sprite: the host provides the atlas image as `bytes`. The index
            // is either inline in the document or supplied as `opts.index` (the
            // fetched sprite `.json` text). Built once into the persistent
            // bank, like brushes/images (unaffected by `clear_sources`).
            SourceDecl::Sprite(sprite) => {
                let atlas = ezu_paint::host::decode_image_bytes(&bytes)
                    .map_err(|e| err(SpriteDecode, format!("atlas decode: {e}")))?;
                let icons = build_sprite_icons(&sprite.index, opts.index.as_deref())
                    .map_err(|e| err(SpriteDecode, e))?;
                self.assets
                    .insert_sprite(sprite.image.clone(), SpriteSheet { atlas, icons });
            }
            // Font: the host provides the raw TTF/OTF/TTC bytes. Built
            // once into the persistent bank keyed by the source's `url`,
            // like brushes/images (unaffected by `clear_sources`).
            SourceDecl::Font(font) => {
                let face = ezu_core::text::Font::from_bytes(bytes.into(), font.index)
                    .map_err(|e| err(FontParse, e))?;
                self.assets.insert_font(font.url.clone(), face);
            }
            // Glyphs: the host provides one raw glyph PBF per call
            // and repeated calls accumulate into one persistent stack.
            // Each glyph is filed under its own id, so a message is
            // free to be a subset spanning several ranges (see
            // `needed_codepoints`) as well as a whole `{range}.pbf`.
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
                            stack.set_byte_budget(self.glyph_budget.unwrap_or(usize::MAX));
                            self.assets.insert_glyphs(key, stack.clone());
                            stack
                        }
                    }
                };
                stack
                    .insert_range(&bytes)
                    .map_err(|e| err(GlyphDecode, e))?;
            }
        }
        Ok(())
    }

    /// Decode a tile-scoped source's bytes — `mvt`, `pmtiles`, `dem`,
    /// `raster`, or remote `geojson` — into a [`SourceData`] that any
    /// number of [`SourceSet`]s can bind without decoding it again.
    ///
    /// Nothing on the renderer changes, so loading is safe from several
    /// threads at once. A host that renders a block of tiles loads each
    /// tile it fetched once and binds it into the set of every tile it
    /// borders, rather than decoding it as the centre of one and again as
    /// a neighbour of each of the others.
    ///
    /// Fails with [`ErrorKind::UnknownSource`] if `name` is not declared or
    /// is a kind that binds into the renderer itself (brush, image, sprite,
    /// font, glyphs — those go through [`bind_source`](Self::bind_source));
    /// with the matching decode kind if the payload does not parse. Unlike
    /// `bind_source`, a DEM or imagery payload that does not decode fails
    /// here and not at render, and the message names the source rather
    /// than a tile, since no tile is known yet.
    pub fn load_source(&self, name: &str, bytes: &[u8]) -> Result<SourceData, Error> {
        let decl = self
            .doc
            .sources
            .get(name)
            .ok_or_else(|| err(UnknownSource, format!("no source `{name}` in style")))?;
        let payload = match decl {
            SourceDecl::Mvt(_) | SourceDecl::Pmtiles(_) => {
                let tile = ezu_features::mvt::decode(bytes).map_err(|e| err(MvtDecode, e))?;
                Payload::Mvt(
                    tile.layers
                        .into_iter()
                        .map(|l| Arc::new(ezu_paint::render::SharedLayer::new(l)))
                        .collect(),
                )
            }
            SourceDecl::Dem(spec) => {
                // The tile id only names the tile in an error message, and
                // `Decode` carries it separately, so drop it from the one
                // reported here.
                let t = decode_dem_tile(bytes, spec.decoding(), 0, 0, 0).map_err(|e| match e {
                    ezu_paint::host::DemDecodeError::Decode { msg, .. } => {
                        err(DemDecode, format!("source `{name}`: {msg}"))
                    }
                })?;
                Payload::Dem(Arc::new(t), spec.decoding())
            }
            SourceDecl::Raster(_) => {
                let buf = ezu_paint::host::decode_image_bytes(bytes)
                    .map_err(|e| err(RasterDecode, format!("source `{name}`: {e}")))?;
                if buf.width != buf.height {
                    return Err(err(
                        RasterDecode,
                        format!(
                            "source `{name}`: non-square tile {}x{}",
                            buf.width, buf.height
                        ),
                    ));
                }
                Payload::Raster(Arc::new(RasterTile {
                    size: buf.width,
                    pixels: buf.pixels,
                }))
            }
            SourceDecl::GeoJson(_) => {
                let doc: serde_json::Value =
                    serde_json::from_slice(bytes).map_err(|e| err(GeoJsonDecode, e))?;
                Payload::GeoJson(Arc::new(doc))
            }
            other => {
                let kind = SourceInfo::new(name, other).kind.name();
                return Err(err(
                    UnknownSource,
                    format!(
                        "source `{name}` is a `{kind}`, which is bound into the renderer \
                         itself with bind_source rather than loaded per tile"
                    ),
                ));
            }
        };
        Ok(SourceData::new(name, payload, bytes))
    }

    /// The tile a host should actually fetch from `name` in order to draw
    /// `z/x/y`, as `(z, x, y)`.
    ///
    /// For a source that declares `max-zoom` (any tile pyramid: `dem`,
    /// `raster`, `mvt`, `pmtiles`), a request past the ceiling answers with
    /// the covering ancestor — bind those bytes with
    /// [`BindOptions::source_zoom`] set to the returned zoom and the
    /// renderer resamples or reprojects them into the requested tile.
    /// Below the ceiling, and for a source that declares no ceiling, the
    /// answer is the tile itself. This exists so the ceiling lives in one
    /// place; a host that hard-codes each source's maxzoom keeps a second
    /// copy of something the style already states, and the two drift.
    ///
    /// The floor is not answered here: below a tile pyramid's `min-zoom`
    /// the render ignores whatever is bound for it, so a host
    /// may skip the fetch, and loses nothing by not skipping it.
    ///
    /// `x` and `y` may be off the tile grid, which is what a host walking
    /// a neighbourhood hands in: `x` wraps around the antimeridian, so
    /// the western neighbour of `z/0/y` is the real tile at the far side
    /// of the world. `y` does not wrap — there is no tile above the north
    /// pole — and an out-of-range row comes back unchanged, so the fetch
    /// for it misses and the stitch clamps that edge, which is what the
    /// pole should look like.
    ///
    /// Fails with [`ErrorKind::UnknownSource`] if `name` is not declared.
    pub fn source_tile(&self, name: &str, z: u8, x: i32, y: i32) -> Result<(u8, i64, i64), Error> {
        let decl = self
            .doc
            .sources
            .get(name)
            .ok_or_else(|| err(UnknownSource, format!("no source `{name}` in style")))?;
        let max_zoom = decl.max_zoom();
        let world = 1i64 << z.min(30);
        let x = (x as i64).rem_euclid(world);
        let y = y as i64;
        Ok(clamp_to_ceiling(max_zoom, z, x, y))
    }

    /// Neighbour tile offsets the active style actually asks for from
    /// `source`, as `(dx, dy)` pairs (never including the centre
    /// `(0, 0)`).
    ///
    /// Cross-tile label collision and edge-continuous DEM/raster shading
    /// are the only things that read neighbours, and only for the sources
    /// they name. Bind each offset listed here and the render gets
    /// exactly what the recipe needs — neither a blind 3×3 per source
    /// nor, more costly, a window short of what a stitched source wanted.
    /// An empty list means the centre tile is enough.
    ///
    /// What comes back depends on the source's kind. A vector source
    /// answers from the graph: a node that wants a neighbour names it,
    /// and cross-tile label collision is usually the only thing that
    /// does, so the list is often empty. A `dem` or `raster` source
    /// answers from its own `neighbor-fetch`, which defaults to on and
    /// means the whole 3×3 — those bind as one stitched canvas, and a
    /// neighbour left unbound is not a missing collision candidate but a
    /// pad filled by clamping the tile's own edge, which shows up as a
    /// seam in whatever samples it. A `geojson` document covers every
    /// tile, so the one bound at the centre also serves each offset listed
    /// for it; binding it there as well changes nothing.
    ///
    /// Fails with [`ErrorKind::UnknownSource`] if `name` is not declared.
    pub fn requested_neighbor_offsets(&self, name: &str) -> Result<Vec<(i32, i32)>, Error> {
        let decl = self
            .doc
            .sources
            .get(name)
            .ok_or_else(|| err(UnknownSource, format!("no source `{name}` in style")))?;
        let requested = self.graph.asset_inputs();
        Ok(ezu_paint::host::source_neighbor_offsets(
            decl, &requested, name,
        ))
    }

    /// Codepoints the currently bound features can require, per glyphs
    /// source, sorted ascending.
    ///
    /// This is the precise form of [`needed_glyph_ranges`](Self::needed_glyph_ranges):
    /// a host that can build its own glyph PBF — one message holding
    /// just these codepoints — transfers only the glyphs the tile draws
    /// instead of the whole 256-codepoint block around each of them. On
    /// CJK labels that is the difference between a few thousand glyphs
    /// and a few tens of megabytes. [`bind_source`](Self::bind_source)
    /// files each glyph by its own id, so such a subset may span any
    /// number of blocks and needs no particular `range` string.
    ///
    /// It is an over-approximation, deliberately: a codepoint is listed
    /// if *any* feature in a text layer carries it in a property the
    /// layer's `text` expression reads, without evaluating filters, zoom
    /// ranges, or the expression itself, and it is listed for every
    /// fontstack in that layer's fallback chain. So it never omits
    /// something a label needs, and it may name a few that go unused.
    ///
    /// An Arabic letter contributes the presentation-form codepoints as
    /// well as its own: it is drawn as whichever of its joined shapes
    /// the letters around it call for, and those shapes live in
    /// U+FB50‥U+FDFF and U+FE70‥U+FEFF.
    pub fn needed_codepoints(&self) -> Result<BTreeMap<&str, Vec<u32>>, Error> {
        Ok(self.needed_glyph_units(&self.bindings, |cp| cp))
    }

    /// [`needed_codepoints`](Self::needed_codepoints) for the features in
    /// `set` rather than the ones bound to the renderer.
    pub fn needed_codepoints_in(&self, set: &SourceSet) -> BTreeMap<&str, Vec<u32>> {
        self.needed_glyph_units(set, |cp| cp)
    }

    /// Glyph ranges the currently bound features can require, per glyphs
    /// source, as range starts (`0`, `256`, `512`, …) — i.e. the
    /// `{range}` in a `…/{fontstack}/{range}.pbf` URL is
    /// `<start>-<start + 255>`.
    ///
    /// A range holds 256 codepoints and a tile typically draws a handful
    /// of them, so this is a coarse unit to fetch in; it is for hosts
    /// that can only fetch whole `{range}.pbf` files off a MapLibre
    /// glyphs endpoint. Hosts that can assemble their own subset PBF
    /// want [`needed_codepoints`](Self::needed_codepoints) instead. Both
    /// see the same set of codepoints and carry the same
    /// over-approximation caveat.
    pub fn needed_glyph_ranges(&self) -> Result<BTreeMap<&str, Vec<u32>>, Error> {
        Ok(self.needed_glyph_units(&self.bindings, |cp| cp & !0xFF))
    }

    /// [`needed_glyph_ranges`](Self::needed_glyph_ranges) for the features
    /// in `set` rather than the ones bound to the renderer.
    pub fn needed_glyph_ranges_in(&self, set: &SourceSet) -> BTreeMap<&str, Vec<u32>> {
        self.needed_glyph_units(set, |cp| cp & !0xFF)
    }

    /// What this renderer is holding, in bytes.
    pub fn memory_usage(&self) -> MemoryUsage {
        let (glyph_ranges, glyph_bytes) = self
            .assets
            .glyphs
            .read()
            .expect("glyphs bank poisoned")
            .values()
            .fold((0usize, 0usize), |(ranges, bytes), stack| {
                let (r, b) = stack.loaded_size();
                (ranges + r, bytes + b)
            });
        let image_bytes: usize = self
            .assets
            .images
            .values()
            .map(|img| img.pixels.len())
            .chain(
                self.assets
                    .sprites
                    .values()
                    .map(|sheet| sheet.atlas.pixels.len()),
            )
            .sum();
        let font_bytes: usize = self.assets.fonts.values().map(|f| f.byte_size()).sum();
        MemoryUsage {
            glyph_bytes,
            glyph_ranges,
            glyph_budget: self.glyph_budget,
            font_bytes,
            image_bytes,
            cache_bytes: self.cache.bytes(),
            cache_budget: self.cache.byte_budget(),
        }
    }

    /// Render a single tile using whatever sources are currently bound.
    ///
    /// Afterwards it trims the glyph banks to the
    /// [glyph budget](Self::set_glyph_budget), as
    /// [`trim_glyphs`](Self::trim_glyphs) does: at once, or, when renders
    /// from sets are running on other threads, as the last of them
    /// finishes, so it never takes glyphs one of them is drawing with.
    pub fn render_tile(
        &self,
        z: u8,
        x: u32,
        y: u32,
        opts: RenderOptions,
    ) -> Result<Vec<u8>, Error> {
        let out = self.render_tile_with(z, x, y, &self.bindings, opts);
        // Trim here rather than at bind time: this tile is done with its
        // glyphs, so dropping the coldest ranges now cannot cost it any.
        // A failed render trims too — it held the same glyphs.
        self.trim_glyphs();
        out
    }

    /// Render a single tile from the tile-scoped data in `set`, ignoring
    /// whatever [`bind_source`](Self::bind_source) has bound for
    /// [`render_tile`](Self::render_tile).
    ///
    /// It reads `set` and the renderer's style and asset banks and changes
    /// nothing but the render cache, which is locked internally, so renders
    /// of different tiles can run on several threads over one `&Renderer`.
    /// It does not apply [`set_glyph_budget`](Self::set_glyph_budget) after
    /// itself, so a host with a budget calls
    /// [`trim_glyphs`](Self::trim_glyphs) when it wants the banks trimmed;
    /// a trim waits for the renders running alongside to finish rather
    /// than drop glyphs from under them.
    pub fn render_tile_with(
        &self,
        z: u8,
        x: u32,
        y: u32,
        set: &SourceSet,
        opts: RenderOptions,
    ) -> Result<Vec<u8>, Error> {
        self.render_from(z, x, y, set, opts, None)
    }

    /// [`render_tile_with`](Self::render_tile_with), also handing back what
    /// each of `nodes` — ids of nodes that produce features — held when the
    /// render finished, in the order given.
    ///
    /// The values are the ones the render computed on its way to the
    /// pixels, so this costs no second evaluation, and the image is the
    /// one `render_tile_with` returns. Each is in the tile's own units; see
    /// [`NodeFeatures`], whose
    /// [`to_geojson_in`](NodeFeatures::to_geojson_in) writes them in
    /// longitude and latitude or in the rendered tile's pixels instead. A
    /// node the render never reached comes back with no features.
    ///
    /// Fails with [`ErrorKind::InvalidStyle`] when a name is not a node of
    /// the style, or names a node that produces something other than
    /// features — checked before anything renders.
    pub fn render_tile_with_features(
        &self,
        z: u8,
        x: u32,
        y: u32,
        set: &SourceSet,
        opts: RenderOptions,
        nodes: &[&str],
    ) -> Result<(Vec<u8>, Vec<NodeFeatures>), Error> {
        let ixs = nodes
            .iter()
            .map(|node| self.features_node(node))
            .collect::<Result<Vec<_>, _>>()?;
        let capture = features::Capture::new(ixs);
        // Pixel coordinates are the image's, so a denser render's are too.
        let tile_size = device_canvas(
            opts.tile_size.unwrap_or(self.render_tile_size()),
            0,
            opts.pixel_ratio,
        )?
        .tile_w;
        let image = self.render_from(z, x, y, set, opts, Some(&capture))?;
        let features = nodes
            .iter()
            .zip(capture.take())
            .map(|(node, features)| NodeFeatures {
                node: node.to_string(),
                features,
                tile: CoreTileId::new(z, x, y),
                tile_size,
            })
            .collect();
        Ok((image, features))
    }

    /// The graph index of `node`, checked to be a node that produces
    /// features.
    fn features_node(&self, node: &str) -> Result<ezu_graph::NodeIx, Error> {
        let ix = self
            .graph
            .index_of(node)
            .ok_or_else(|| err(InvalidStyle, format!("the style has no node `{node}`")))?;
        let kind = self.graph.output_kind(ix);
        if kind != ezu_graph::PortKind::Features {
            return Err(err(
                InvalidStyle,
                format!("node `{node}` produces {kind}, not features"),
            ));
        }
        Ok(ix)
    }

    /// A collector for the page features `req` asks of `frame`, for a host
    /// that renders the frame's tiles itself; see
    /// [`FrameFeatureCollector`].
    /// [`render_frame_with_features`](Self::render_frame_with_features)
    /// makes and drives one of its own.
    ///
    /// Fails with [`ErrorKind::InvalidStyle`] when a name is not a node of
    /// the style, names one that produces something other than features,
    /// or a key names no property — checked here, before anything renders.
    pub fn frame_features(
        &self,
        frame: &Frame,
        req: &FrameFeatures,
    ) -> Result<FrameFeatureCollector, Error> {
        for entry in &req.nodes {
            self.features_node(&entry.node)?;
        }
        frame_features::check_request(req)?;
        Ok(FrameFeatureCollector::new(frame, req))
    }

    /// [`render_tile_with`](Self::render_tile_with), showing every node's
    /// value to `observer` as the evaluator computes it or finds it in the
    /// cache, with how long it took. A debugging hook: `ezu graph --tile`
    /// draws each node's picture onto the graph from it.
    ///
    /// Only in Rust. The values are the evaluator's own in-memory types —
    /// rasters, fields, brushes — and a callback per node into the host,
    /// neither of which has a form across the wasm boundary the JS and Go
    /// packages sit behind. What a host outside a debugger wants from a
    /// node, its features, crosses that boundary through
    /// [`render_tile_with_features`](Self::render_tile_with_features).
    /// Node indices are those of [`graph`](Self::graph).
    pub fn render_tile_observed(
        &self,
        z: u8,
        x: u32,
        y: u32,
        set: &SourceSet,
        opts: RenderOptions,
        observer: &dyn ezu_graph::NodeObserver,
    ) -> Result<Vec<u8>, Error> {
        self.render_from(z, x, y, set, opts, Some(observer))
    }

    /// Bring every bound fontstack back under the
    /// [glyph budget](Self::set_glyph_budget). A no-op until a host sets
    /// one. [`render_tile`](Self::render_tile) does this after each
    /// render; a host rendering with
    /// [`render_tile_with`](Self::render_tile_with) calls it when it likes.
    ///
    /// It never drops glyphs from under a render: called while renders run
    /// on other threads, it leaves the trim to the last of them to finish,
    /// so the banks shrink once nothing is reading them.
    pub fn trim_glyphs(&self) {
        if self.glyph_budget.is_none() {
            return;
        }
        let trimming = match self.glyph_gate.try_write() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                // Say a trim is owed, then look again: a render that
                // finished in between did not see the flag, but its lock
                // is gone; one still running will see it when it ends.
                self.trim_pending.store(true, Ordering::SeqCst);
                match self.glyph_gate.try_write() {
                    Ok(guard) => guard,
                    Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                    Err(std::sync::TryLockError::WouldBlock) => return,
                }
            }
        };
        self.trim_pending.store(false, Ordering::SeqCst);
        for stack in self
            .assets
            .glyphs
            .read()
            .expect("glyphs bank poisoned")
            .values()
        {
            stack.trim_to_budget();
        }
        drop(trimming);
    }

    /// Shared body of the prepass calls: the needed codepoints per glyphs
    /// source, for the features in `set`, mapped through `unit` (identity,
    /// or the range start containing it) and deduped.
    fn needed_glyph_units(
        &self,
        set: &SourceSet,
        unit: fn(u32) -> u32,
    ) -> BTreeMap<&str, Vec<u32>> {
        let mut out = BTreeMap::new();
        for (source, codepoints) in self.needed_codepoints_by_source(set) {
            let mut units: Vec<u32> = codepoints.into_iter().map(unit).collect();
            units.dedup();
            out.insert(source, units);
        }
        out
    }

    /// Glyphs source name → the BMP codepoints its fontstacks can be
    /// asked to shape, over-approximated as documented on
    /// [`needed_glyph_ranges`](Self::needed_glyph_ranges).
    fn needed_codepoints_by_source(&self, set: &SourceSet) -> BTreeMap<&str, BTreeSet<u32>> {
        let mut needed: BTreeMap<&str, BTreeSet<u32>> = BTreeMap::new();

        // Every bound vector tile, centre and neighbours alike.
        let decoded: Vec<(&str, &[Arc<ezu_paint::render::SharedLayer>])> = set
            .entries
            .iter()
            .flat_map(|(name, slots)| {
                slots.values().filter_map(move |p| match &p.data.payload {
                    Payload::Mvt(layers) => Some((&**name, &**layers)),
                    _ => None,
                })
            })
            .collect();
        // Every GeoJSON document the labels can read: the ones bound in the
        // set, and the ones the style carries inline as `data`. A geojson
        // source binds a single layer named after itself.
        let geojson: Vec<(&str, &serde_json::Value)> = set
            .entries
            .iter()
            .flat_map(|(name, slots)| {
                slots.values().filter_map(move |p| match &p.data.payload {
                    Payload::GeoJson(doc) => Some((&**name, &**doc)),
                    _ => None,
                })
            })
            .chain(
                self.doc
                    .sources
                    .iter()
                    .filter_map(|(name, decl)| match decl {
                        SourceDecl::GeoJson(s) => s.data.as_ref().map(|d| (name.as_str(), d)),
                        _ => None,
                    }),
            )
            .collect();
        for spec in self.doc.nodes.values() {
            let Some(stacks) = glyph_source_names(&self.doc, &spec.fields) else {
                continue;
            };
            let Some(text) = spec.fields.get("text") else {
                continue;
            };
            let mut codepoints: BTreeSet<u32> = BTreeSet::new();
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
                for &(bound_source, layers) in &decoded {
                    if source.is_some_and(|s| s != bound_source) {
                        continue;
                    }
                    for l in layers.iter().map(|l| &l.layer) {
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
                for &(bound_source, doc) in &geojson {
                    if source.is_some_and(|s| s != bound_source)
                        || layer.is_some_and(|want| want != bound_source)
                    {
                        continue;
                    }
                    for_each_geojson_properties(doc, &mut |properties| {
                        for p in &props {
                            if let Some(serde_json::Value::String(s)) = properties.get(*p) {
                                collect_codepoints(s, &mut codepoints);
                            }
                        }
                    });
                }
            }
            for stack in stacks {
                needed.entry(stack).or_default().extend(codepoints.iter());
            }
        }
        needed
    }

    /// [`render_reading`](Self::render_reading), holding the glyph banks
    /// against a trim for as long as it runs, and doing a trim that had to
    /// wait for it once it is done.
    fn render_from(
        &self,
        z: u8,
        x: u32,
        y: u32,
        set: &SourceSet,
        opts: RenderOptions,
        observer: Option<&dyn ezu_graph::NodeObserver>,
    ) -> Result<Vec<u8>, Error> {
        let out = {
            let _reading = self
                .glyph_gate
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.render_reading(z, x, y, set, opts, observer)
        };
        if self.trim_pending.load(Ordering::SeqCst) {
            self.trim_glyphs();
        }
        out
    }

    /// Binds each payload in `set` to a per-render loader, dispatching on
    /// the source's declared kind in the style, and renders, showing each
    /// node's value to `observer` as it is computed.
    fn render_reading(
        &self,
        z: u8,
        x: u32,
        y: u32,
        set: &SourceSet,
        opts: RenderOptions,
        observer: Option<&dyn ezu_graph::NodeObserver>,
    ) -> Result<Vec<u8>, Error> {
        let tile_size = opts.tile_size.unwrap_or(self.render_tile_size());
        // `opts.pad` is an explicit override. Otherwise the style's `pad`
        // acts as a floor under what the graph actually reaches, so a
        // margin too narrow for the filters cannot silently clamp the
        // tile's edge pixels.
        let pad = match opts.pad {
            Some(pad) => pad,
            None => self.doc.pad.max(
                self.graph
                    .required_pad()
                    .map_err(|e| err(InvalidStyle, e))?,
            ),
        };
        let tile_id = GraphTileId { z, x, y };
        let canvas = device_canvas(tile_size, pad, opts.pixel_ratio)?;
        let mut tile_loader = TileLoader::new(&self.assets, tile_id);
        let requested = self.graph.asset_inputs();

        // `on-missing: error` is the style saying a tile it cannot draw
        // should fail rather than come out blank. Natively the fetcher
        // raises it; here the host does the fetching, so the equivalent
        // is "nothing was bound for a source the graph reads". A missing
        // *neighbour* still degrades — the stitch edge-clamps, same as
        // the native host.
        for (name, decl) in &self.doc.sources {
            if decl.on_missing() != Some(OnMissing::Error) {
                continue;
            }
            // Below the source's `min-zoom` there is no tile to miss.
            if decl.min_zoom().is_some_and(|mz| z < mz) {
                continue;
            }
            // A vector source's layers are read as `<source>.<layer>`.
            let read = match decl {
                SourceDecl::Mvt(_) | SourceDecl::Pmtiles(_) => requested.iter().any(|r| {
                    r.strip_prefix(name.as_str())
                        .is_some_and(|rest| rest.starts_with('.'))
                }),
                _ => requested.contains(name.as_str()),
            };
            if !read {
                continue;
            }
            let has_centre = set.get(name).is_some_and(|m| m.contains_key(&(0, 0)));
            if !has_centre {
                return Err(err(
                    UnknownSource,
                    format!(
                        "source `{name}` declares `on-missing: error` and has nothing bound \
                         for {z}/{x}/{y}; bind the tile with coord [0, 0] (or its ancestor \
                         with sourceZoom), or answer the request as missing"
                    ),
                ));
            }
        }

        // A `dem` or `raster` stitches its 3×3 into one canvas-sized
        // buffer, and a neighbour the host did not bind is not a gap —
        // it is pad filled by clamping the centre tile's own edge, which
        // every filter that samples the pad then reads as real. That
        // renders, so nothing else here would say a word about it. Say
        // it: what the render is short of is exactly what
        // `requested_neighbor_offsets` names.
        let world = 1i64 << z.min(30);
        for (name, decl) in &self.doc.sources {
            // Vector neighbours are collision candidates, not pixels;
            // binding only the centre is a documented degrade.
            if !matches!(decl, SourceDecl::Dem(_) | SourceDecl::Raster(_)) {
                continue;
            }
            // Below `min-zoom` whatever is bound is ignored anyway.
            if decl.min_zoom().is_some_and(|mz| z < mz) {
                continue;
            }
            let Some(bound) = set.get(name) else {
                continue;
            };
            let wanted = ezu_paint::host::source_neighbor_offsets(decl, &requested, name);
            // A row off the top or bottom of the world has no tile to
            // bind, and clamping there is the right answer.
            let missing = wanted
                .iter()
                .filter(|&&(dx, dy)| {
                    let row = y as i64 + dy as i64;
                    (0..world).contains(&row) && !bound.contains_key(&(dx, dy))
                })
                .count();
            if missing > 0 {
                tracing::warn!(
                    "source `{name}`: {missing} of the {} neighbour tiles it asks for are \
                     unbound for {z}/{x}/{y} — the pad they would fill is clamped from the \
                     centre tile's edge, and whatever samples it will seam at the tile border. \
                     Bind every offset `requestedNeighborOffsets(\"{name}\")` reports.",
                    wanted.len(),
                );
            }
        }

        let here = CoreTileId::new(z, x, y);
        for (name, slots) in &set.entries {
            let name: &str = name;
            // Dispatch on what the active style declares, not on what was
            // loaded: data loaded under another style may not match it.
            let Some(decl) = self.doc.sources.get(name) else {
                return Err(err(
                    UnknownSource,
                    format!("source `{name}` is bound but not declared in the active style"),
                ));
            };
            let mismatch = || {
                err(
                    UnknownSource,
                    format!(
                        "source `{name}` was loaded as another kind than the active style \
                         declares it; load it again under this style"
                    ),
                )
            };
            // Below a tile pyramid's `min-zoom` it has nothing to show, so
            // what is bound for it is left unread: its `features` draw
            // nothing, its `raster` is transparent, its `dem` is flat.
            if decl.min_zoom().is_some_and(|mz| z < mz) {
                continue;
            }
            match decl {
                SourceDecl::Mvt(_) | SourceDecl::Pmtiles(_) => {
                    // Centre under `<source>.<layer>`, any neighbours the
                    // host bound under `@dx,dy` (cross-tile collision). A
                    // host binding only the centre degrades to centre-only
                    // collision at borders — no error.
                    for (&(dx, dy), placed) in slots {
                        let Payload::Mvt(layers) = &placed.data.payload else {
                            return Err(mismatch());
                        };
                        // Overzoom: the data is an ancestor's, so put its
                        // geometry into this tile's frame first. Each
                        // neighbour resolves against its own ancestor,
                        // which may or may not be the centre's.
                        let oz = resolve_overzoom(here, dx, dy, placed.source_zoom)
                            .map_err(|e| err(UnknownSource, e))?;
                        match oz.from {
                            Some((ancestor, shift)) => {
                                let clipped = layers
                                    .iter()
                                    .map(|l| {
                                        ezu_features::mvt::clip_layer_to_descendant(
                                            &l.layer, ancestor, oz.target,
                                        )
                                        .map(Arc::new)
                                    })
                                    .collect::<Result<Vec<_>, _>>()
                                    .map_err(|e| err(MvtDecode, e))?;
                                tile_loader.bind_mvt_layers(name, dx, dy, clipped, shift);
                            }
                            // The same layers every render that binds this
                            // data shares, uncopied, along with their
                            // converted features.
                            None => {
                                tile_loader.bind_shared_layers(
                                    name,
                                    dx,
                                    dy,
                                    layers.iter().cloned(),
                                );
                            }
                        }
                        // Overzoom yields the same layer names, so the
                        // source's own layers name every binding made.
                        for layer in layers.iter() {
                            let base = format!("{name}.{}", layer.layer.name);
                            tile_loader.mix_content_hash(
                                &ezu_graph::neighbor_binding(&base, dx, dy),
                                placed.data.content,
                            );
                        }
                    }
                }
                SourceDecl::Dem(spec) => {
                    let mut tiles: HashMap<(i32, i32), Arc<DemTile>> =
                        HashMap::with_capacity(slots.len());
                    for (&(dx, dy), placed) in slots {
                        let oz = resolve_overzoom(here, dx, dy, placed.source_zoom)
                            .map_err(|e| err(UnknownSource, e))?;
                        let t = match &placed.data.payload {
                            Payload::Dem(t, encoding) if *encoding == spec.decoding() => t.clone(),
                            Payload::DemBytes(bytes) => {
                                // Decode errors name the tile the host
                                // actually fetched, which under overzoom is
                                // the ancestor.
                                let src = oz.from.map_or(oz.target, |(a, _)| a);
                                let t =
                                    decode_dem_tile(bytes, spec.decoding(), src.z, src.x, src.y)
                                        .map_err(|e| err(DemDecode, e))?;
                                Arc::new(t)
                            }
                            _ => return Err(mismatch()),
                        };
                        // Overzoom: resample the ancestor's sub-rectangle
                        // covering this tile, matching what the native
                        // host does for a source past its `max-zoom`.
                        let t = match oz.from {
                            Some((a, shift)) => Arc::new(upsample_subregion(
                                &t,
                                shift,
                                oz.target.x,
                                oz.target.y,
                                a.x,
                                a.y,
                            )),
                            None => t,
                        };
                        tiles.insert((dx, dy), t);
                    }
                    let borrowed: HashMap<(i32, i32), &DemTile> =
                        tiles.iter().map(|(k, v)| (*k, &**v)).collect();
                    let field =
                        stitch_padded_field(&borrowed, spec.elevation_offset, tile_id, canvas)
                            .ok_or_else(|| {
                                err(
                                    DemDecode,
                                    format!(
                                        "source `{name}`: missing centre tile (bind with coord \
                                         [0, 0])"
                                    ),
                                )
                            })?;
                    // Bare source name — the `dem` node's asset lookup key.
                    tile_loader.bind_scalar_field(name, field);
                    tile_loader.mix_content_hash(name, stitched_content(slots));
                }
                SourceDecl::Raster(spec) => {
                    let mut tiles: HashMap<(i32, i32), Arc<RasterTile>> =
                        HashMap::with_capacity(slots.len());
                    for (&(dx, dy), placed) in slots {
                        let oz = resolve_overzoom(here, dx, dy, placed.source_zoom)
                            .map_err(|e| err(UnknownSource, e))?;
                        let t = match &placed.data.payload {
                            Payload::Raster(t) => t.clone(),
                            Payload::RasterBytes(bytes) => {
                                let src = oz.from.map_or(oz.target, |(a, _)| a);
                                let t = decode_raster_tile(bytes, src.z, src.x, src.y)
                                    .map_err(|e| err(RasterDecode, e))?;
                                Arc::new(t)
                            }
                            _ => return Err(mismatch()),
                        };
                        let t = match oz.from {
                            Some((a, shift)) => Arc::new(upsample_subregion_raster(
                                &t,
                                shift,
                                oz.target.x,
                                oz.target.y,
                                a.x,
                                a.y,
                                spec.resampling,
                            )),
                            None => t,
                        };
                        tiles.insert((dx, dy), t);
                    }
                    let borrowed: HashMap<(i32, i32), &RasterTile> =
                        tiles.iter().map(|(k, v)| (*k, &**v)).collect();
                    let buf = stitch_padded_raster(&borrowed, canvas, spec.resampling).ok_or_else(
                        || {
                            err(
                                RasterDecode,
                                format!(
                                    "source `{name}`: missing centre tile (bind with coord [0, 0])"
                                ),
                            )
                        },
                    )?;
                    tile_loader.bind_raster(name, buf);
                    tile_loader.mix_content_hash(name, stitched_content(slots));
                }
                SourceDecl::GeoJson(_) => {
                    for (&(dx, dy), placed) in slots {
                        let Payload::GeoJson(data) = &placed.data.payload else {
                            return Err(mismatch());
                        };
                        ezu_paint::host::bind_geojson(&mut tile_loader, name, data, dx, dy)
                            .map_err(|e| err(GeoJsonDecode, e))?;
                        tile_loader.mix_content_hash(
                            &ezu_graph::neighbor_binding(&format!("{name}.{name}"), dx, dy),
                            placed.data.content,
                        );
                    }
                    // A GeoJSON document covers the whole world, so the one
                    // bound at the centre is every neighbour's document too.
                    // Project it into each neighbour the graph reads and the
                    // host left unbound. Without them a label anchored in a
                    // neighbour has no candidate here, and the part of it
                    // that reaches across the seam is not drawn.
                    if let Some(centre) = slots.get(&(0, 0)) {
                        let Payload::GeoJson(data) = &centre.data.payload else {
                            return Err(mismatch());
                        };
                        for (dx, dy) in
                            ezu_paint::host::requested_neighbor_offsets(&requested, name)
                        {
                            if slots.contains_key(&(dx, dy)) {
                                continue;
                            }
                            ezu_paint::host::bind_geojson(&mut tile_loader, name, data, dx, dy)
                                .map_err(|e| err(GeoJsonDecode, e))?;
                            tile_loader.mix_content_hash(
                                &ezu_graph::neighbor_binding(&format!("{name}.{name}"), dx, dy),
                                centre.data.content,
                            );
                        }
                    }
                }
                _ => return Err(mismatch()),
            }
        }

        // Inline GeoJSON needs no binding — the document carries it, so
        // project it straight into this tile. Anything bound remotely above
        // is already done and drops out here.
        let mut inline = ezu_paint::host::GeoJsonSources::inline(&self.doc);
        inline.retain(|name| !set.contains(name));
        ezu_paint::host::bind_geojson_sources(&mut tile_loader, &inline, &requested)
            .map_err(|e| err(GeoJsonDecode, e))?;

        // Validated against the document's declarations by the same
        // parser `--param` uses, so a bad value fails here rather than
        // rendering something quietly wrong.
        let mut params = ParamValues::new();
        for (name, raw) in opts.params.clone() {
            let value = parse_param_value(&self.doc.params, &name, &raw)
                .map_err(|e| err(InvalidStyle, e))?;
            params.set(name, value);
        }

        let ev = Evaluator::new(&self.graph, &self.cache, &tile_loader);
        let ev = match observer {
            Some(o) => ev.with_observer(o),
            None => ev,
        };
        encode_render(ev, tile_id, canvas, opts, &params)
    }
}

/// The identity of a 3×3 stitch: which payload sits at each offset, and
/// from which zoom it is resampled. Offsets are sorted so the answer does
/// not depend on the map's iteration order.
fn stitched_content(slots: &HashMap<(i32, i32), source_set::Placed>) -> u128 {
    let mut offsets: Vec<_> = slots.keys().copied().collect();
    offsets.sort_unstable();
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    for (dx, dy) in offsets {
        let placed = &slots[&(dx, dy)];
        h.update(&dx.to_le_bytes());
        h.update(&dy.to_le_bytes());
        h.update(&placed.data.content.to_le_bytes());
        h.update(&[
            u8::from(placed.source_zoom.is_some()),
            placed.source_zoom.unwrap_or(0),
        ]);
    }
    h.digest128()
}

/// A render cache holding up to `budget` bytes of pixels, or the graph
/// cache's default.
fn new_cache(budget: Option<usize>) -> Arc<Cache> {
    Arc::new(Cache::with_limits(
        ezu_graph::cache::DEFAULT_CAPACITY,
        budget.unwrap_or(ezu_graph::cache::DEFAULT_BYTE_BUDGET),
    ))
}

fn already_bound_otherwise(name: &str) -> Error {
    err(
        UnknownSource,
        format!("source `{name}` already bound as a different kind"),
    )
}

/// The canvas a render of `tile_size` style pixels with a `pad` style
/// pixel margin draws on at `pixel_ratio`.
///
/// The tile rounds to a whole number of device pixels, and the ratio the
/// nodes scale their px fields by is the one that rounding lands on, so
/// a width and the geometry it is drawn along always agree. The margin
/// rounds up: one narrower than asked would clip what the filters reach.
fn device_canvas(tile_size: u32, pad: u32, pixel_ratio: f32) -> Result<CanvasInfo, Error> {
    if !(1.0..=MAX_PIXEL_RATIO).contains(&pixel_ratio) {
        return Err(err(
            InvalidStyle,
            format!("pixel ratio must be between 1 and {MAX_PIXEL_RATIO}, not {pixel_ratio}"),
        ));
    }
    if pixel_ratio == 1.0 {
        return Ok(CanvasInfo::square(tile_size, pad));
    }
    let device = (tile_size as f64 * pixel_ratio as f64).round().max(1.0) as u32;
    let ratio = device as f32 / tile_size as f32;
    let canvas = CanvasInfo::square(device, 0).with_pixel_ratio(ratio);
    Ok(CanvasInfo {
        pad: canvas.px_ceil(pad),
        ..canvas
    })
}

/// Shared finish step: evaluate the graph and encode the result.
fn encode_render(
    ev: Evaluator<'_>,
    tile_id: GraphTileId,
    canvas: CanvasInfo,
    opts: RenderOptions,
    params: &ParamValues,
) -> Result<Vec<u8>, Error> {
    let seed = tile_seed(tile_id.z, tile_id.x, tile_id.y);
    // The parallel evaluator is used only when the caller opts in, which
    // a host does exactly when it has a thread pool. Before that — and in
    // every single-threaded build, where `render_parallel` transparently
    // falls back to `render` — we stay sequential: `render_parallel`
    // would otherwise touch rayon's global pool, which can't be built on
    // wasm without workers. Both paths are deterministic and produce
    // identical output.
    let out = if opts.parallel {
        ev.render_parallel(tile_id, canvas, params, seed)
    } else {
        ev.render(tile_id, canvas, params, seed)
    }
    .map_err(|e| err(RenderFailed, e))?;
    let raster = match out {
        PortValue::Raster(r) => r,
        other => {
            return Err(err(
                RenderFailed,
                format!("expected Raster output, got {:?}", other.kind()),
            ))
        }
    };
    // Crop by both axes: a tile canvas has them equal, and going through
    // the general entry points means a canvas that does not stays right.
    let (cw, ch) = (canvas.tile_w, canvas.tile_h);
    Ok(match opts.format {
        OutputFormat::Png => crop_to_png(&raster, cw, ch, canvas.pad, opts.png_compression)
            .map_err(|e| err(PngEncode, e))?,
        OutputFormat::Webp => {
            crop_to_webp(&raster, cw, ch, canvas.pad).map_err(|e| err(WebpEncode, e))?
        }
        OutputFormat::Rgba => crop_to_rgba8(&raster, cw, ch, canvas.pad),
    })
}

/// The tile to fetch for `z/x/y` given a source's `max-zoom`: shift right
/// by the overshoot, and leave tiles at or above the ceiling alone.
fn clamp_to_ceiling(max_zoom: Option<u8>, z: u8, x: i64, y: i64) -> (u8, i64, i64) {
    match max_zoom {
        Some(mz) if z > mz => (mz, x >> (z - mz), y >> (z - mz)),
        _ => (z, x, y),
    }
}

/// Where one bound payload came from: the tile it belongs to, and — when
/// the host is overzooming — the ancestor it was actually fetched at.
#[derive(Debug, PartialEq, Eq)]
struct Overzoom {
    /// The tile these bytes are for: the render's tile, or the neighbour
    /// at `(dx, dy)`.
    target: CoreTileId,
    /// `Some((ancestor, shift))` when the bytes are an ancestor's and the
    /// payload must be resampled into `target`. `None` when they are
    /// already in the target's frame.
    from: Option<(CoreTileId, u8)>,
}

/// Resolve a bound payload's provenance, shared by every tile-scoped
/// source kind so a host states overzoom the same way for all of them.
///
/// `source_zoom` equal to the target's zoom needs no transform, and one
/// deeper than the target is refused — there is no detail to invent.
fn resolve_overzoom(
    here: CoreTileId,
    dx: i32,
    dy: i32,
    source_zoom: Option<u8>,
) -> Result<Overzoom, String> {
    let target = neighbor_tile(here, dx, dy).ok_or_else(|| {
        format!(
            "coord [{dx}, {dy}] is off the map at zoom {}, so it has no ancestor to \
             overzoom from",
            here.z
        )
    })?;
    let Some(sz) = source_zoom else {
        return Ok(Overzoom { target, from: None });
    };
    if sz > target.z {
        return Err(format!(
            "sourceZoom {sz} is deeper than the requested zoom {}; overzoom only \
             reprojects downwards",
            target.z
        ));
    }
    // `ancestor_at` is `None` at equal zoom, which is exactly the "these
    // bytes already fit this tile" case.
    let from = target.ancestor_at(sz).map(|a| (a, target.z - sz));
    Ok(Overzoom { target, from })
}

/// The tile `(dx, dy)` away from `tile`, or `None` when that lands off
/// the map. `x` wraps at the antimeridian, as tile schemes do; `y` does
/// not, since there is nothing above the north edge or below the south.
fn neighbor_tile(tile: CoreTileId, dx: i32, dy: i32) -> Option<CoreTileId> {
    let axis = i64::from(tile.axis_tiles());
    let x = (i64::from(tile.x) + i64::from(dx)).rem_euclid(axis);
    let y = i64::from(tile.y) + i64::from(dy);
    if y < 0 || y >= axis {
        return None;
    }
    Some(CoreTileId::new(tile.z, x as u32, y as u32))
}

fn parse_and_build(style_json: &str) -> Result<(Document, Graph), Error> {
    let doc = Document::from_json(style_json).map_err(|e| err(InvalidStyle, e))?;
    let registry = default_registry();
    let graph = build_graph(&doc, &registry).map_err(|e| err(InvalidStyle, e))?;
    for w in graph.warnings() {
        tracing::warn!("{w}");
    }
    Ok((doc, graph))
}

fn tile_seed(z: u8, x: u32, y: u32) -> u64 {
    let mut s = 0u64;
    s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(z as u64);
    s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(x as u64);
    s = s.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(y as u64);
    s
}

/// The `glyphs` source names a node's `font` stack resolves to, or
/// `None` when the node has no font stack (so it is not a text node).
///
/// A `font` entry names a source; only the ones declared as `glyphs`
/// have ranges to bind, and their *source* name is what `bind_source`
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

/// Call `visit` with the `properties` of every Feature in a GeoJSON
/// document: a FeatureCollection's features, or a lone Feature. A bare
/// geometry has no properties to label with.
fn for_each_geojson_properties(
    doc: &serde_json::Value,
    visit: &mut dyn FnMut(&serde_json::Map<String, serde_json::Value>),
) {
    match doc.get("type").and_then(|t| t.as_str()) {
        Some("FeatureCollection") => {
            for f in doc
                .get("features")
                .and_then(|fs| fs.as_array())
                .into_iter()
                .flatten()
            {
                for_each_geojson_properties(f, visit);
            }
        }
        Some("Feature") => {
            if let Some(properties) = doc.get("properties").and_then(|p| p.as_object()) {
                visit(properties);
            }
        }
        _ => {}
    }
}

/// Add the codepoint of every character in `s`. Codepoints outside
/// the Basic Multilingual Plane are skipped: the glyph protocol cannot
/// address them, so nothing can serve them.
fn collect_codepoints(s: &str, out: &mut BTreeSet<u32>) {
    for c in s.chars() {
        let cp = c as u32;
        if cp <= 0xFFFF {
            out.insert(cp);
        }
        // An Arabic letter is drawn from a glyph stack as one of its
        // presentation forms, chosen by the letters around it, so the
        // codepoints the label needs are those and not only the letter
        // it is written with. Which form depends on context this
        // prepass does not have, so all of them are listed.
        out.extend(
            ezu_core::text::presentation_forms(c)
                .into_iter()
                .map(|f| f as u32),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trim never takes glyphs from under a render on another thread: it
    /// waits for the last render running to finish, which does it.
    #[test]
    fn a_trim_waits_for_the_renders_in_flight() {
        let mut r = Renderer::new(
            r##"{ "name": "labels", "tile-size": 64,
                  "sources": { "g": { "type": "glyphs", "url": "https://example.invalid/{fontstack}/{range}.pbf",
                                      "fontstack": "Sans" } },
                  "nodes": { "out": { "op": "solid", "color": "#000000" } },
                  "output": "@out" }"##,
        )
        .expect("builds");
        let range = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../ezu-core/tests/glyphs/0-255.pbf"),
        )
        .expect("reads the glyph range");
        r.bind_source("g", range, &BindOptions::default())
            .expect("binds");
        r.set_glyph_budget(Some(1));
        assert_eq!(r.memory_usage().glyph_ranges, 1);

        // As if a render were running on another thread.
        let reading = r.glyph_gate.read().unwrap();
        r.trim_glyphs();
        assert_eq!(r.memory_usage().glyph_ranges, 1, "trimmed under a render");
        drop(reading);

        // The next render to finish does the trim it was owed.
        r.render_tile_with(1, 0, 0, &SourceSet::new(), RenderOptions::default())
            .expect("renders");
        assert_eq!(r.memory_usage().glyph_ranges, 0, "the owed trim never ran");
        assert!(!r.trim_pending.load(Ordering::SeqCst));
    }

    /// The warnings are the graph's, and follow the style.
    #[test]
    fn the_build_warnings_are_the_current_styles() {
        // A `raster` node that names no source falls back to the only one
        // declared, and says so when the graph is built.
        let mut r = Renderer::new(
            r##"{ "name": "unnamed",
                  "sources": { "sky": { "type": "raster",
                                        "url": "https://example.invalid/{z}/{x}/{y}.png" } },
                  "nodes": { "out": { "op": "raster" } },
                  "output": "@out" }"##,
        )
        .expect("builds");
        assert!(
            r.warnings()
                .iter()
                .any(|w| w.contains("`source` is not set")),
            "{:?}",
            r.warnings()
        );
        assert_eq!(r.warnings(), r.graph().warnings());
        r.set_style(
            r##"{ "name": "plain", "nodes": { "out": { "op": "solid", "color": "#000000" } },
                  "output": "@out" }"##,
        )
        .expect("builds");
        assert!(r.warnings().is_empty());
        assert_eq!(r.graph().len(), 1);
    }

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
        let mut out = BTreeSet::new();
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
    fn an_arabic_letter_also_needs_its_presentation_forms() {
        let mut out = BTreeSet::new();
        // Waw is drawn joined or not depending on what precedes it, so
        // both of its shapes are needed alongside the letter itself.
        collect_codepoints("\u{0648}", &mut out);
        assert_eq!(
            out.iter().copied().collect::<Vec<_>>(),
            vec![0x0648, 0xFEED, 0xFEEE]
        );
        // A label that needs them names the blocks they live in, which
        // is what a host fetching whole ranges binds.
        let mut starts: Vec<u32> = out.iter().map(|cp| cp & !0xFF).collect();
        starts.dedup();
        assert_eq!(starts, vec![0x0600, 0xFE00]);
    }

    #[test]
    fn a_neighbor_wraps_in_x_and_stops_in_y() {
        // Overzoom resolves each neighbour against its *own* ancestor, so
        // the neighbour coordinate has to be right before the ancestor
        // can be. At zoom 2 the axis is 4 tiles.
        let centre = CoreTileId::new(2, 0, 1);
        assert_eq!(neighbor_tile(centre, 1, 0), Some(CoreTileId::new(2, 1, 1)));
        // West of column 0 is the far east column, not -1.
        assert_eq!(neighbor_tile(centre, -1, 0), Some(CoreTileId::new(2, 3, 1)));
        assert_eq!(
            neighbor_tile(CoreTileId::new(2, 3, 1), 1, 0),
            Some(CoreTileId::new(2, 0, 1))
        );
        // There is no tile above the north edge or below the south.
        assert_eq!(neighbor_tile(CoreTileId::new(2, 1, 0), 0, -1), None);
        assert_eq!(neighbor_tile(CoreTileId::new(2, 1, 3), 0, 1), None);
    }

    #[test]
    fn neighbors_of_one_tile_can_want_different_ancestors() {
        // The 3x3 window around a z16 tile straddles two z15 parents, so
        // binding one parent's bytes for every neighbour and clipping
        // per-neighbour is the whole point of resolving ancestors
        // individually.
        let centre = CoreTileId::new(16, 100, 200);
        let parent = centre.ancestor_at(15).expect("z15 is shallower");
        assert_eq!(parent, CoreTileId::new(15, 50, 100));
        let east = neighbor_tile(centre, 1, 0).expect("in range");
        assert_eq!(east.ancestor_at(15), Some(CoreTileId::new(15, 50, 100)));
        let further = neighbor_tile(centre, 2, 0).expect("in range");
        assert_eq!(further.ancestor_at(15), Some(CoreTileId::new(15, 51, 100)));
        // Equal zoom has no ancestor, which is how the render path spots
        // "these bytes already fit this tile".
        assert_eq!(centre.ancestor_at(16), None);
    }

    #[test]
    fn overzoom_resolves_each_offset_against_its_own_ancestor() {
        let here = CoreTileId::new(16, 100, 200);
        // Centre and its eastern neighbour share a z15 parent; two tiles
        // east falls into the next one. Every source kind resolves this
        // the same way, which is what keeps the DEM stitch aligned with
        // the vector tile drawn over it.
        let centre = resolve_overzoom(here, 0, 0, Some(15)).expect("in range");
        assert_eq!(centre.target, here);
        assert_eq!(centre.from, Some((CoreTileId::new(15, 50, 100), 1)));
        let east = resolve_overzoom(here, 1, 0, Some(15)).expect("in range");
        assert_eq!(
            east.from.map(|(a, _)| a),
            Some(CoreTileId::new(15, 50, 100))
        );
        let further = resolve_overzoom(here, 2, 0, Some(15)).expect("in range");
        assert_eq!(
            further.from.map(|(a, _)| a),
            Some(CoreTileId::new(15, 51, 100))
        );
    }

    #[test]
    fn overzoom_is_a_no_op_at_the_rendered_zoom() {
        // A host that passes the source's maxzoom unconditionally must
        // not pay for a resample on tiles at or above it.
        let here = CoreTileId::new(15, 50, 100);
        assert_eq!(resolve_overzoom(here, 0, 0, Some(15)).unwrap().from, None);
        assert_eq!(resolve_overzoom(here, 0, 0, None).unwrap().from, None);
    }

    #[test]
    fn overzoom_refuses_a_deeper_source_zoom() {
        let here = CoreTileId::new(12, 10, 20);
        let err = resolve_overzoom(here, 0, 0, Some(14)).unwrap_err();
        assert!(err.contains("deeper than the requested zoom"), "got {err}");
    }

    #[test]
    fn a_source_tile_answer_clamps_to_the_declared_ceiling() {
        // `source_tile` is where the ceiling lives, so the arithmetic is
        // worth pinning: shift right by the overshoot, and leave shallow
        // tiles alone.
        assert_eq!(
            clamp_to_ceiling(Some(14), 22, 3_728_270, 1_649_855),
            (14, 14563, 6444)
        );
        assert_eq!(
            clamp_to_ceiling(Some(14), 14, 14563, 6444),
            (14, 14563, 6444)
        );
        assert_eq!(clamp_to_ceiling(Some(14), 10, 909, 402), (10, 909, 402));
        assert_eq!(
            clamp_to_ceiling(None, 22, 3_728_270, 1_649_855),
            (22, 3_728_270, 1_649_855)
        );
        // The clamped answer is the ancestor the render path resolves to,
        // so a host can feed one straight into the other.
        let (tz, tx, ty) = clamp_to_ceiling(Some(14), 16, 58252, 25776);
        let oz =
            resolve_overzoom(CoreTileId::new(16, 58252, 25776), 0, 0, Some(tz)).expect("in range");
        assert_eq!(
            oz.from.map(|(a, _)| (a.z, a.x as i64, a.y as i64)),
            Some((tz, tx, ty))
        );
    }

    /// A vector style over the Go package's z14 fixture tile, with `zoom`
    /// spliced into the source's declaration.
    fn vector_style(zoom: &str) -> String {
        format!(
            r##"{{
                "name": "test",
                "tile-size": 64,
                "sources": {{ "basemap": {{ "type": "mvt", "url": "https://x/{{z}}/{{x}}/{{y}}.mvt" {zoom} }} }},
                "nodes": {{
                    "f": {{ "op": "features", "source": "basemap", "layer": "earth" }},
                    "out": {{ "op": "fill-solid", "features": "@f", "fill": "#000000" }}
                }},
                "output": "out"
            }}"##
        )
    }

    const BASEMAP_Z14: &[u8] = include_bytes!("../../../../go/testdata/basemap-14-14554-6454.mvt");

    #[test]
    fn a_vector_source_overzooms_past_its_declared_max_zoom() {
        let r = Renderer::new(&vector_style(r#", "max-zoom": 14"#)).expect("builds");
        assert_eq!(
            r.source_tile("basemap", 16, 58218, 25817).unwrap(),
            (14, 14554, 6454)
        );
        let r = Renderer::new(&vector_style("")).expect("builds");
        assert_eq!(
            r.source_tile("basemap", 16, 58218, 25817).unwrap(),
            (16, 58218, 25817)
        );
    }

    #[test]
    fn a_vector_source_draws_nothing_below_its_declared_min_zoom() {
        let ink = |zoom: &str| {
            let mut r = Renderer::new(&vector_style(zoom)).expect("builds");
            r.bind_source("basemap", BASEMAP_Z14.to_vec(), &BindOptions::default())
                .expect("binds");
            let opts = RenderOptions {
                format: OutputFormat::Rgba,
                ..RenderOptions::default()
            };
            let px = r.render_tile(14, 14554, 6454, opts).expect("renders");
            px.chunks(4).filter(|p| p[3] > 0).count()
        };
        assert!(ink("") > 0, "the fixture has land to fill");
        assert!(
            ink(r#", "min-zoom": 14"#) > 0,
            "the floor itself is in range"
        );
        assert_eq!(ink(r#", "min-zoom": 15"#), 0);
    }

    #[test]
    fn a_vector_source_with_on_missing_error_fails_a_tile_left_unbound() {
        let render = |extra: &str, bind: bool| {
            let mut r = Renderer::new(&vector_style(extra)).expect("builds");
            if bind {
                r.bind_source("basemap", BASEMAP_Z14.to_vec(), &BindOptions::default())
                    .expect("binds");
            }
            r.render_tile(14, 14554, 6454, RenderOptions::default())
        };
        let error = r#", "on-missing": "error""#;
        assert!(render(error, true).is_ok());
        let e = render(error, false).expect_err("nothing bound");
        assert!(e.to_string().contains("on-missing: error"), "{e}");
        // Without it a missing tile is simply empty, as it always was.
        assert!(render("", false).is_ok());
        assert!(render(r#", "on-missing": "empty""#, false).is_ok());
        // Below the floor there is no tile to miss.
        assert!(render(r#", "on-missing": "error", "min-zoom": 15"#, false).is_ok());
    }

    #[test]
    fn a_raster_source_draws_nothing_below_its_declared_min_zoom() {
        // A green tile to bind, made by rendering one.
        let painter = Renderer::new(
            r##"{ "name": "green", "tile-size": 8,
                  "nodes": { "out": { "op": "solid", "color": "#00ff00" } },
                  "output": "out" }"##,
        )
        .expect("builds");
        let green = painter
            .render_tile(0, 0, 0, RenderOptions::default())
            .expect("renders");

        let mut r = Renderer::new(
            r##"{
                "name": "test",
                "tile-size": 8,
                "sources": { "photo": { "type": "raster", "url": "https://x/{z}/{x}/{y}.png",
                                        "min-zoom": 3, "on-missing": "error",
                                        "neighbor-fetch": false } },
                "nodes": { "out": { "op": "raster", "source": "photo" } },
                "output": "out"
            }"##,
        )
        .expect("builds");
        let opts = || RenderOptions {
            format: OutputFormat::Rgba,
            ..RenderOptions::default()
        };
        let mut ink = |z: u8, bind: bool| {
            if bind {
                r.bind_source("photo", green.clone(), &BindOptions::default())
                    .expect("binds");
            }
            let px = r.render_tile(z, 0, 0, opts());
            r.clear_sources();
            px.map(|px| px.chunks(4).filter(|p| p[3] > 0).count())
        };
        assert!(ink(3, true).unwrap() > 0, "the floor itself is in range");
        // Below the floor the bound tile is left unread, and a missing one
        // is no error: the source has no tiles there to miss.
        assert_eq!(ink(2, true).unwrap(), 0);
        assert_eq!(ink(2, false).unwrap(), 0);
        assert!(
            ink(3, false).is_err(),
            "on-missing: error still holds in range"
        );
    }

    #[test]
    fn a_set_tile_size_renders_what_the_edited_style_does() {
        // A blur reaches past the tile, so the pad and the neighbour-bleed
        // crop are part of what has to come out the same.
        let style = |tile_size: u32| {
            format!(
                r##"{{
                    "name": "test",
                    "tile-size": {tile_size},
                    "sources": {{ "basemap": {{ "type": "mvt", "url": "https://x/{{z}}/{{x}}/{{y}}.mvt" }} }},
                    "nodes": {{
                        "f": {{ "op": "features", "source": "basemap", "layer": "earth" }},
                        "fill": {{ "op": "fill-solid", "features": "@f", "fill": "#204080" }},
                        "out": {{ "op": "blur", "input": "@fill", "sigma": 3.0 }}
                    }},
                    "output": "out"
                }}"##
            )
        };
        let render = |r: &mut Renderer| {
            r.bind_source("basemap", BASEMAP_Z14.to_vec(), &BindOptions::default())
                .expect("binds");
            let opts = RenderOptions {
                format: OutputFormat::Rgba,
                ..RenderOptions::default()
            };
            let px = r.render_tile(14, 14554, 6454, opts).expect("renders");
            r.clear_sources();
            px
        };

        let mut edited = Renderer::new(&style(93)).expect("builds");
        let want = render(&mut edited);
        assert_eq!(want.len(), 93 * 93 * 4);

        let mut r = Renderer::new(&style(64)).expect("builds");
        let at_style = render(&mut r);
        r.set_tile_size(Some(93)).expect("a real size");
        assert_eq!((r.tile_size(), r.render_tile_size()), (64, 93));
        assert!(
            render(&mut r) == want,
            "rendering at 93 px differs from the style edited to 93 px"
        );

        // Back to the style's, and the cache dropped on the way there and
        // back holds nothing that changes the answer.
        r.set_tile_size(None).expect("clearing");
        assert_eq!(r.render_tile_size(), 64);
        assert!(render(&mut r) == at_style);

        // A new style starts at its own size.
        r.set_tile_size(Some(93)).expect("a real size");
        r.set_style(&style(64)).expect("builds");
        assert_eq!(r.render_tile_size(), 64);

        assert_eq!(
            r.set_tile_size(Some(0)).unwrap_err().kind(),
            ErrorKind::InvalidStyle
        );
        assert_eq!(r.render_tile_size(), 64, "a refused size changes nothing");
    }

    #[test]
    fn ranges_cover_each_codepoint_block_once() {
        let mut out = BTreeSet::new();
        collect_codepoints("AZ", &mut out);
        collect_codepoints("東京", &mut out);
        // 'A' = U+0041 and 'Z' = U+005A share block 0; '東' = U+6771 →
        // 0x6700; '京' = U+4EAC → 0x4E00.
        let mut starts: Vec<u32> = out.iter().map(|cp| cp & !0xFF).collect();
        starts.dedup();
        assert_eq!(starts, vec![0x0000, 0x4E00, 0x6700]);
    }

    #[test]
    fn declared_sources_carry_their_kind_and_where_to_fetch_them() {
        // The whole point of the call: a host reads the bind loop off this
        // and never looks at the style itself. So every field a loop
        // branches on is pinned here — the kind, whether it survives
        // `clear_sources`, and the address, including the two shapes that
        // are not simply "the url as written".
        let r = Renderer::new(
            r##"{
                "name": "test",
                "tile-size": 8,
                "sources": {
                    "basemap": { "type": "mvt", "url": "https://x/{z}/{x}/{y}.mvt" },
                    "terrain": { "type": "dem", "url": "https://x/dem/{z}/{x}/{y}.png",
                                 "encoding": "terrarium" },
                    "labels": { "type": "glyphs",
                                "url": "https://x/fonts/{fontstack}/{range}.pbf",
                                "fontstack": "Noto Sans Regular" },
                    "icons": { "type": "sprite", "image": "https://x/sprite.png",
                               "index": "https://x/sprite.json" },
                    "here": { "type": "geojson",
                              "data": { "type": "FeatureCollection", "features": [] } }
                },
                "nodes": { "bg": { "op": "solid", "color": "#ffffff" } },
                "output": "bg"
            }"##,
        )
        .expect("the style builds");

        let got: Vec<String> = r
            .sources()
            .iter()
            .map(|s| {
                format!(
                    "{} {} tile-scoped={} url={:?} index={:?}",
                    s.name,
                    s.kind.name(),
                    s.kind.is_tile_scoped(),
                    s.url.as_deref().unwrap_or("-"),
                    s.index_url.unwrap_or("-"),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                "basemap mvt tile-scoped=true url=\"https://x/{z}/{x}/{y}.mvt\" index=\"-\"",
                "terrain dem tile-scoped=true url=\"https://x/dem/{z}/{x}/{y}.png\" index=\"-\"",
                // `{fontstack}` is substituted and percent-encoded here so
                // the host does not have to know how MapLibre spells it;
                // `{range}` stays, since it is fetched per block.
                "labels glyphs tile-scoped=false \
                 url=\"https://x/fonts/Noto%20Sans%20Regular/{range}.pbf\" index=\"-\"",
                // A sprite is two fetches: the atlas, and the index the
                // atlas is bound with.
                "icons sprite tile-scoped=false url=\"https://x/sprite.png\" \
                 index=\"https://x/sprite.json\"",
                // Inline geojson has nothing to fetch and nothing to bind.
                "here geojson tile-scoped=true url=\"-\" index=\"-\"",
            ],
            "sources come back in declaration order"
        );
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

    #[test]
    fn every_error_kind_keeps_its_wire_name() {
        // Hosts branch on these strings, and the JS shell's README
        // documents them. Renaming one silently breaks a caller.
        for (kind, name) in [
            (InvalidStyle, "InvalidStyle"),
            (BrushParse, "BrushParse"),
            (MvtDecode, "MvtDecode"),
            (DemDecode, "DemDecode"),
            (RasterDecode, "RasterDecode"),
            (GeoJsonDecode, "GeoJsonDecode"),
            (SpriteDecode, "SpriteDecode"),
            (FontParse, "FontParse"),
            (GlyphDecode, "GlyphDecode"),
            (RenderFailed, "RenderFailed"),
            (PngEncode, "PngEncode"),
            (WebpEncode, "WebpEncode"),
            (UnknownSource, "UnknownSource"),
            (OutOfMemory, "OutOfMemory"),
        ] {
            assert_eq!(kind.name(), name);
        }
        // The message is the detail alone: a shell puts the name in its
        // own slot (a JS `Error`'s `.name`) and must not see it twice.
        let e = Error::new(MvtDecode, "invalid tag value: 0");
        assert_eq!(e.name(), "MvtDecode");
        assert_eq!(e.to_string(), "invalid tag value: 0");
    }

    #[test]
    fn a_pixel_ratio_scales_the_tile_and_its_margin() {
        let c = device_canvas(256, 10, 1.0).expect("plain");
        assert_eq!((c.tile_w, c.pad, c.pixel_ratio), (256, 10, 1.0));
        let c = device_canvas(256, 10, 2.0).expect("retina");
        assert_eq!(
            (c.tile_w, c.tile_h, c.pad, c.pixel_ratio),
            (512, 512, 20, 2.0)
        );
        // A fraction rounds the tile, and the ratio follows the rounding so
        // widths and geometry agree; the margin rounds up.
        let c = device_canvas(100, 3, 1.333).expect("fractional");
        assert_eq!((c.tile_w, c.pad), (133, 4));
        assert_eq!(c.pixel_ratio, 1.33);
        for bad in [0.5, 0.0, 4.5, f32::NAN, f32::INFINITY] {
            let e = device_canvas(256, 0, bad).expect_err("refused");
            assert_eq!(e.kind(), InvalidStyle, "{bad}");
        }
    }

    #[test]
    fn a_retina_render_is_twice_the_size_and_draws_widths_twice_as_wide() {
        let r = Renderer::new(
            r##"{ "name": "line", "tile-size": 32,
                  "nodes": {
                    "l": { "op": "literal-geometry", "lines": [[[0, 2048], [4096, 2048]]] },
                    "s": { "op": "stroke", "features": "@l", "width-px": 4, "color": "#000000" }
                  },
                  "output": "s" }"##,
        )
        .expect("builds");
        let rows = |ratio: f32| {
            let px = r
                .render_tile(
                    0,
                    0,
                    0,
                    RenderOptions {
                        format: OutputFormat::Rgba,
                        pixel_ratio: ratio,
                        ..RenderOptions::default()
                    },
                )
                .expect("renders");
            let side = ((px.len() / 4) as f64).sqrt() as usize;
            let inked = (0..side)
                .filter(|y| px[(y * side + side / 2) * 4 + 3] > 128)
                .count();
            (side, inked)
        };
        assert_eq!(rows(1.0), (32, 4));
        assert_eq!(rows(2.0), (64, 8));
    }
}
