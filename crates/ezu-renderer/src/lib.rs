//! The host-neutral body of ezu's embedding shells.
//!
//! An embedding shell — the wasm-bindgen module, a C ABI for a wazero
//! host — owns all I/O and speaks its host's dialect: JS objects,
//! JSON strings, C pointers. Everything underneath that is the same
//! renderer, and it lives here, so a second shell is a translation
//! layer rather than a second renderer.
//!
//! [`Renderer`] holds a parsed style document, its built graph, an
//! in-memory asset bank, and a per-tile binding buffer that mirrors the
//! style's `sources` block. The flow mirrors the CLI's: bind every
//! source the style declares, render, clear. [`Renderer::sources`] says
//! what those are and where to fetch them, so a host drives the loop
//! without reading the style itself.
//!
//! ```no_run
//! # use ezu_renderer::{BindOptions, RenderOptions, Renderer};
//! # fn fetch_mvt(z: u8, x: u32, y: u32) -> Vec<u8> { unimplemented!() }
//! # fn main() -> Result<(), ezu_renderer::Error> {
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
//! This host cannot fetch anything itself, so everything a render needs
//! must be bound before it: [`Renderer::requested_neighbor_offsets`]
//! names the neighbour tiles, and [`Renderer::needed_codepoints`] the
//! glyphs.
//!
//! Every fallible call returns an [`Error`] carrying an [`ErrorKind`]
//! whose [`name`](ErrorKind::name) is what a host branches on.

mod error;
mod options;
mod sources;

pub use error::{Error, ErrorKind};
pub use options::{BindOptions, OutputFormat, RenderOptions};
pub use sources::{SourceInfo, SourceKind};

/// Effort the PNG encoder spends, named by
/// [`RenderOptions::png_compression`]. Re-exported so a shell needs this
/// crate and nothing under it.
pub use ezu_paint::host::PngCompression;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use ezu_core::TileId as CoreTileId;
use ezu_graph::{
    build_graph, parse_param_value, Cache, CanvasInfo, Evaluator, Graph, ParamValues, PortValue,
    SpriteSheet, TileId,
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

/// Pending tile bytes for a single named source. MVT bytes are
/// validated at bind time (we attempt a decode and discard the
/// result) so errors surface immediately, but we keep the raw bytes
/// because `DecodedTile` isn't `Clone` and rendering wants to bind
/// freshly. DEM stays as raw bytes per `(dx, dy)` neighbour offset
/// until render time, when the centre tile id is known.
enum SourceBinding {
    /// Raw MVT bytes per `(dx, dy)` neighbour offset. The centre tile is
    /// `coord: (0, 0)` (default); neighbours feed cross-tile label
    /// collision and are bound under `@dx,dy` names at render time.
    /// Re-decoded at render because `DecodedTile` isn't Clone.
    Mvt(HashMap<(i32, i32), BoundBytes>),
    Dem(HashMap<(i32, i32), BoundBytes>),
    /// RGBA imagery tiles per `(dx, dy)` neighbour offset, decoded +
    /// stitched at render time like DEM.
    Raster(HashMap<(i32, i32), BoundBytes>),
    /// Raw GeoJSON bytes (WGS84 lon/lat) per `(dx, dy)` neighbour offset,
    /// projected into each tile frame at render time. Only needed for
    /// *remote* geojson; inline `data` is read straight from the document.
    GeoJson(HashMap<(i32, i32), Vec<u8>>),
}

/// One bound tile payload and, when the host is overzooming, the zoom the
/// bytes are natively encoded at. Shared by every tile-scoped source kind
/// — vector geometry is reprojected, DEM and imagery are resampled, but
/// the host states the same thing in the same way for all of them.
struct BoundBytes {
    bytes: Vec<u8>,
    /// `Some(z)` when these bytes belong to an ancestor of the tile being
    /// rendered — a source that stops at `maxzoom` while the host serves
    /// deeper tiles. `None` means the bytes are already in the requested
    /// tile's frame, which is the ordinary case.
    source_zoom: Option<u8>,
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
    /// Pending source bindings, keyed by the `sources.<name>` entry in
    /// the style. Cleared by [`Renderer::clear_sources`].
    bindings: HashMap<String, SourceBinding>,
    /// Ceiling on resident glyph bytes per fontstack, applied after each
    /// render. `None` (the default) keeps every bound range.
    glyph_budget: Option<usize>,
}

impl Renderer {
    /// Build a renderer from a style JSON document.
    pub fn new(style_json: &str) -> Result<Self, Error> {
        let (doc, graph) = parse_and_build(style_json)?;
        Ok(Self {
            doc,
            graph: Arc::new(graph),
            cache: Arc::new(Cache::new()),
            assets: BrushBankLoader::new(),
            bindings: HashMap::new(),
            glyph_budget: None,
        })
    }

    /// Replace the active style. Returns the new node count. Invalidates
    /// the intermediate cache and drops any pending source bindings.
    pub fn set_style(&mut self, style_json: &str) -> Result<usize, Error> {
        let (doc, graph) = parse_and_build(style_json)?;
        let n = doc.nodes.len();
        self.doc = doc;
        self.graph = Arc::new(graph);
        self.cache = Arc::new(Cache::new());
        self.bindings.clear();
        Ok(n)
    }

    /// `tile-size` declared by the current style.
    pub fn tile_size(&self) -> u32 {
        self.doc.tile_size
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
            .filter(|n| self.bindings.contains_key(n.as_str()))
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
            SourceDecl::Mvt(_) | SourceDecl::Pmtiles(_) => {
                // Validate now so a malformed payload fails at bind
                // time rather than render time — we toss the result and
                // re-decode at render since `DecodedTile` isn't Clone.
                let _ = ezu_features::mvt::decode(&bytes).map_err(|e| err(MvtDecode, e))?;
                let entry = self
                    .bindings
                    .entry(name.to_string())
                    .or_insert_with(|| SourceBinding::Mvt(HashMap::new()));
                let SourceBinding::Mvt(map) = entry else {
                    return Err(already_bound_otherwise(name));
                };
                map.insert(
                    opts.coord,
                    BoundBytes {
                        bytes,
                        source_zoom: opts.source_zoom,
                    },
                );
            }
            SourceDecl::Dem(_) => {
                let entry = self
                    .bindings
                    .entry(name.to_string())
                    .or_insert_with(|| SourceBinding::Dem(HashMap::new()));
                let SourceBinding::Dem(map) = entry else {
                    return Err(already_bound_otherwise(name));
                };
                map.insert(
                    opts.coord,
                    BoundBytes {
                        bytes,
                        source_zoom: opts.source_zoom,
                    },
                );
            }
            SourceDecl::Raster(_) => {
                let entry = self
                    .bindings
                    .entry(name.to_string())
                    .or_insert_with(|| SourceBinding::Raster(HashMap::new()));
                let SourceBinding::Raster(map) = entry else {
                    return Err(already_bound_otherwise(name));
                };
                map.insert(
                    opts.coord,
                    BoundBytes {
                        bytes,
                        source_zoom: opts.source_zoom,
                    },
                );
            }
            // Remote GeoJSON: the host fetched the document and hands the
            // raw bytes here. (Inline `data` needs no bind — it's read from
            // the document at render time.) Stored tile-scoped; projected per
            // tile when the render runs.
            SourceDecl::GeoJson(_) => {
                // Validate now so malformed bytes fail at bind time.
                serde_json::from_slice::<serde_json::Value>(&bytes)
                    .map_err(|e| err(GeoJsonDecode, e))?;
                let entry = self
                    .bindings
                    .entry(name.to_string())
                    .or_insert_with(|| SourceBinding::GeoJson(HashMap::new()));
                let SourceBinding::GeoJson(map) = entry else {
                    return Err(already_bound_otherwise(name));
                };
                map.insert(opts.coord, bytes);
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

    /// The tile a host should actually fetch from `name` in order to draw
    /// `z/x/y`, as `(z, x, y)`.
    ///
    /// For a source that declares `max-zoom`, a request past the ceiling
    /// answers with the covering ancestor — bind those bytes with
    /// [`BindOptions::source_zoom`] set to the returned zoom and the
    /// renderer resamples or reprojects them into the requested tile.
    /// Below the ceiling, and for a source that declares no ceiling, the
    /// answer is the tile itself. This exists so the ceiling lives in one
    /// place; a host that hard-codes each source's maxzoom keeps a second
    /// copy of something the style already states, and the two drift.
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
        let max_zoom = match decl {
            SourceDecl::Dem(s) => s.max_zoom,
            SourceDecl::Raster(s) => s.max_zoom,
            _ => None,
        };
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
    /// seam in whatever samples it.
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
        self.needed_glyph_units(|cp| cp)
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
        self.needed_glyph_units(|cp| cp & !0xFF)
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
    pub fn render_tile(
        &self,
        z: u8,
        x: u32,
        y: u32,
        opts: RenderOptions,
    ) -> Result<Vec<u8>, Error> {
        let out = self.render_with_bindings(z, x, y, opts);
        // Trim here rather than at bind time: this tile is done with its
        // glyphs, so dropping the coldest ranges now cannot cost it any.
        // A failed render trims too — it held the same glyphs.
        self.trim_glyphs();
        out
    }

    /// Bring every bound fontstack back under the glyph budget. A no-op
    /// until a host sets one with `set_glyph_budget`.
    fn trim_glyphs(&self) {
        if self.glyph_budget.is_none() {
            return;
        }
        for stack in self
            .assets
            .glyphs
            .read()
            .expect("glyphs bank poisoned")
            .values()
        {
            stack.trim_to_budget();
        }
    }

    /// Decode every bound MVT source once, centre and neighbours alike,
    /// for inspection ahead of a render.
    fn decode_bound_features(&self) -> Result<Vec<(&str, ezu_features::mvt::DecodedTile)>, Error> {
        let mut out = Vec::new();
        for (name, binding) in &self.bindings {
            let SourceBinding::Mvt(byte_map) = binding else {
                continue;
            };
            for payload in byte_map.values() {
                let tile =
                    ezu_features::mvt::decode(&payload.bytes).map_err(|e| err(MvtDecode, e))?;
                out.push((name.as_str(), tile));
            }
        }
        Ok(out)
    }

    /// Shared body of the two prepass calls: the needed codepoints per
    /// glyphs source, mapped through `unit` (identity, or the range
    /// start containing it) and deduped.
    fn needed_glyph_units(&self, unit: fn(u32) -> u32) -> Result<BTreeMap<&str, Vec<u32>>, Error> {
        let mut out = BTreeMap::new();
        for (source, codepoints) in self.needed_codepoints_by_source()? {
            let mut units: Vec<u32> = codepoints.into_iter().map(unit).collect();
            units.dedup();
            out.insert(source, units);
        }
        Ok(out)
    }

    /// Glyphs source name → the BMP codepoints its fontstacks can be
    /// asked to shape, over-approximated as documented on
    /// [`needed_glyph_ranges`](Self::needed_glyph_ranges).
    fn needed_codepoints_by_source(&self) -> Result<BTreeMap<&str, BTreeSet<u32>>, Error> {
        let mut needed: BTreeMap<&str, BTreeSet<u32>> = BTreeMap::new();

        let decoded = self.decode_bound_features()?;
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

    /// Reads pending source bindings from `self.bindings` and dispatches
    /// each based on its declared kind in the style.
    fn render_with_bindings(
        &self,
        z: u8,
        x: u32,
        y: u32,
        opts: RenderOptions,
    ) -> Result<Vec<u8>, Error> {
        let tile_size = opts.tile_size.unwrap_or(self.doc.tile_size);
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
        let tile_id = TileId { z, x, y };
        let canvas = CanvasInfo::square(tile_size, pad);
        let mut tile_loader = TileLoader::new(&self.assets, tile_id);
        let requested = self.graph.asset_inputs();

        // `on-missing: error` is the style saying a tile it cannot draw
        // should fail rather than come out blank. Natively the fetcher
        // raises it; here the host does the fetching, so the equivalent
        // is "nothing was bound for a source the graph reads". A missing
        // *neighbour* still degrades — the stitch edge-clamps, same as
        // the native host.
        for (name, decl) in &self.doc.sources {
            let on_missing = match decl {
                SourceDecl::Dem(s) => s.on_missing,
                SourceDecl::Raster(s) => s.on_missing,
                _ => continue,
            };
            if on_missing != OnMissing::Error || !requested.contains(name.as_str()) {
                continue;
            }
            let has_centre = match self.bindings.get(name) {
                Some(SourceBinding::Dem(m)) | Some(SourceBinding::Raster(m)) => {
                    m.contains_key(&(0, 0))
                }
                _ => false,
            };
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
            let bound = match self.bindings.get(name) {
                Some(SourceBinding::Dem(m)) | Some(SourceBinding::Raster(m)) => m,
                // Vector neighbours are collision candidates, not pixels;
                // binding only the centre is a documented degrade.
                _ => continue,
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

        for (name, binding) in &self.bindings {
            match binding {
                SourceBinding::Mvt(byte_map) => {
                    // Centre under `<source>.<layer>`, any neighbours the
                    // host bound under `@dx,dy` (cross-tile collision). A
                    // host binding only the centre degrades to centre-only
                    // collision at borders — no error.
                    for (&(dx, dy), payload) in byte_map {
                        let mut decoded = ezu_features::mvt::decode(&payload.bytes)
                            .map_err(|e| err(MvtDecode, e))?;
                        // Overzoom: the bytes are an ancestor's, so put
                        // their geometry into this tile's frame first.
                        // Each neighbour resolves against its own
                        // ancestor, which may or may not be the centre's.
                        let here = CoreTileId::new(tile_id.z, tile_id.x, tile_id.y);
                        let oz = resolve_overzoom(here, dx, dy, payload.source_zoom)
                            .map_err(|e| err(UnknownSource, e))?;
                        if let Some((ancestor, _)) = oz.from {
                            decoded = ezu_features::mvt::clip_to_descendant(
                                &decoded, ancestor, oz.target,
                            )
                            .map_err(|e| err(MvtDecode, e))?;
                        }
                        tile_loader.bind_mvt_neighbor(name, dx, dy, decoded);
                    }
                }
                SourceBinding::Dem(byte_map) => {
                    let encoding = match self.doc.sources.get(name) {
                        Some(SourceDecl::Dem(spec)) => spec.encoding,
                        _ => {
                            return Err(err(
                                UnknownSource,
                                format!("source `{name}` is no longer a DEM in the active style"),
                            ));
                        }
                    };
                    let elevation_offset = match self.doc.sources.get(name) {
                        Some(SourceDecl::Dem(spec)) => spec.elevation_offset,
                        _ => 0.0,
                    };
                    let here = CoreTileId::new(z, x, y);
                    let mut decoded: HashMap<(i32, i32), DemTile> =
                        HashMap::with_capacity(byte_map.len());
                    for (&(dx, dy), payload) in byte_map {
                        let oz = resolve_overzoom(here, dx, dy, payload.source_zoom)
                            .map_err(|e| err(UnknownSource, e))?;
                        // Decode errors name the tile the host actually
                        // fetched, which under overzoom is the ancestor.
                        let src = oz.from.map_or(oz.target, |(a, _)| a);
                        let t = decode_dem_tile(&payload.bytes, encoding, src.z, src.x, src.y)
                            .map_err(|e| err(DemDecode, e))?;
                        // Overzoom: resample the ancestor's sub-rectangle
                        // covering this tile, matching what the native
                        // host does for a source past its `max-zoom`.
                        let t = match oz.from {
                            Some((a, shift)) => {
                                upsample_subregion(&t, shift, oz.target.x, oz.target.y, a.x, a.y)
                            }
                            None => t,
                        };
                        decoded.insert((dx, dy), t);
                    }
                    let borrowed: HashMap<(i32, i32), &DemTile> =
                        decoded.iter().map(|(k, v)| (*k, v)).collect();
                    let field = stitch_padded_field(&borrowed, elevation_offset, tile_id, canvas)
                        .ok_or_else(|| {
                        err(
                            DemDecode,
                            format!(
                                "source `{name}`: missing centre tile (bind with coord [0, 0])"
                            ),
                        )
                    })?;
                    // Bare source name — the `dem` node's asset lookup key.
                    tile_loader.bind_scalar_field(name.clone(), field);
                }
                SourceBinding::Raster(byte_map) => {
                    let here = CoreTileId::new(z, x, y);
                    let mut decoded: HashMap<(i32, i32), RasterTile> =
                        HashMap::with_capacity(byte_map.len());
                    for (&(dx, dy), payload) in byte_map {
                        let oz = resolve_overzoom(here, dx, dy, payload.source_zoom)
                            .map_err(|e| err(UnknownSource, e))?;
                        let src = oz.from.map_or(oz.target, |(a, _)| a);
                        let t = decode_raster_tile(&payload.bytes, src.z, src.x, src.y)
                            .map_err(|e| err(RasterDecode, e))?;
                        let t = match oz.from {
                            Some((a, shift)) => upsample_subregion_raster(
                                &t,
                                shift,
                                oz.target.x,
                                oz.target.y,
                                a.x,
                                a.y,
                            ),
                            None => t,
                        };
                        decoded.insert((dx, dy), t);
                    }
                    let borrowed: HashMap<(i32, i32), &RasterTile> =
                        decoded.iter().map(|(k, v)| (*k, v)).collect();
                    let buf = stitch_padded_raster(&borrowed, canvas).ok_or_else(|| {
                        err(
                            RasterDecode,
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
                            serde_json::from_slice(bytes).map_err(|e| err(GeoJsonDecode, e))?;
                        ezu_paint::host::bind_geojson(&mut tile_loader, name, &data, dx, dy)
                            .map_err(|e| err(GeoJsonDecode, e))?;
                    }
                }
            }
        }

        // Inline GeoJSON needs no binding — the document carries it, so
        // project it straight into this tile. Anything bound remotely above
        // is already done and drops out here.
        let mut inline = ezu_paint::host::GeoJsonSources::inline(&self.doc);
        inline.retain(|name| !self.bindings.contains_key(name));
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

        encode_render(
            &self.graph,
            &self.cache,
            &tile_loader,
            tile_id,
            canvas,
            opts,
            &params,
        )
    }
}

fn already_bound_otherwise(name: &str) -> Error {
    err(
        UnknownSource,
        format!("source `{name}` already bound as a different kind"),
    )
}

/// Shared finish step: evaluate the graph and encode the result.
fn encode_render(
    graph: &Graph,
    cache: &Cache,
    tile_loader: &TileLoader<'_>,
    tile_id: TileId,
    canvas: CanvasInfo,
    opts: RenderOptions,
    params: &ParamValues,
) -> Result<Vec<u8>, Error> {
    let ev = Evaluator::new(graph, cache, tile_loader);
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
}
