//! Host-side glue for rendering: pluggable `AssetLoader` impls and
//! conversion helpers between `ezu_graph::RasterBuf` and `tiny-skia` /
//! PNG output.

pub mod dem_decode;
pub use dem_decode::{decode_dem_tile, stitch_padded_field, DemDecodeError, DemTile};
pub mod raster_decode;
pub use raster_decode::{
    decode_raster_tile, stitch_padded_raster, upsample_subregion_raster, RasterTile,
};

#[cfg(feature = "http")]
pub mod dem;
#[cfg(feature = "http")]
pub use dem::{bind_dem_sources, build_dem_sources, DemFetchError, DemSourceRegistry};
#[cfg(feature = "http")]
pub mod raster;
#[cfg(feature = "http")]
pub use raster::{
    bind_raster_sources, build_raster_sources, RasterFetchError, RasterSourceRegistry,
};
#[cfg(feature = "http")]
mod tilejson;

use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ezu_features::{mvt::DecodedTile, FeatureLayer};
use ezu_graph::{
    Asset, AssetError, AssetLoader, OpaqueValue, RasterBuf, ScalarField, SpriteRect, SpriteSheet,
    TileId,
};
use hokusai::Brush;
use tiny_skia::{Pixmap, PixmapPaint, Transform};
use xxhash_rust::xxh3::Xxh3;

use crate::PaintError;

/// In-memory asset bank for `brush-file` and `image` references.
///
/// Names may be supplied with or without a leading `@`. Resolution
/// order on miss is: in-memory brush bank, then `<brushes_dir>` on
/// disk for `.myb` brushes, then in-memory image bank, then
/// `<images_dir>` on disk for PNGs. The brush-bank/-dir pair is kept
/// for backwards compatibility with the original brush-only API.
///
/// Every loader is pre-populated with the built-in brushes listed in
/// [`crate::builtin::BUILTIN_BRUSHES`] (CC0, bundled into the binary
/// via `include_str!`). Use [`BrushBankLoader::empty`] for a loader
/// without them.
pub struct BrushBankLoader {
    pub bank: HashMap<String, Arc<Brush>>,
    pub brushes_dir: Option<PathBuf>,
    pub images: HashMap<String, Arc<RasterBuf>>,
    pub images_dir: Option<PathBuf>,
    /// Sprite sheets keyed by their atlas `image` src (the same string an
    /// `icon` node's `sprite: "@name"` ref resolves to).
    pub sprites: HashMap<String, Arc<SpriteSheet>>,
    /// Fonts keyed by their source's `url` (the string a `text` node's
    /// `font` stack resolves to).
    pub fonts: HashMap<String, Arc<ezu_core::text::Font>>,
}

impl BrushBankLoader {
    /// New loader with the bundled built-in brushes pre-registered.
    pub fn new() -> Self {
        let mut this = Self::empty();
        this.register_builtins();
        this
    }

    /// New loader with no brushes registered — caller manages the bank.
    pub fn empty() -> Self {
        Self {
            bank: HashMap::new(),
            brushes_dir: None,
            images: HashMap::new(),
            images_dir: None,
            sprites: HashMap::new(),
            fonts: HashMap::new(),
        }
    }

    /// Register every entry in [`crate::builtin::BUILTIN_BRUSHES`].
    /// Entries that fail to parse are silently skipped — the brushes
    /// are bundled at compile time, so a parse failure is a bug in this
    /// crate rather than a runtime condition callers can recover from.
    pub fn register_builtins(&mut self) -> &mut Self {
        for (name, myb_json) in crate::builtin::BUILTIN_BRUSHES {
            if let Ok(brush) = hokusai::myb::from_str(myb_json) {
                self.bank.insert((*name).to_string(), Arc::new(brush));
            }
        }
        self
    }

    pub fn with_dir(mut self, dir: PathBuf) -> Self {
        self.brushes_dir = Some(dir);
        self
    }

    pub fn with_images_dir(mut self, dir: PathBuf) -> Self {
        self.images_dir = Some(dir);
        self
    }

    pub fn insert(&mut self, name: impl Into<String>, brush: Brush) {
        self.bank.insert(name.into(), Arc::new(brush));
    }

    pub fn insert_image(&mut self, name: impl Into<String>, image: RasterBuf) {
        self.images.insert(name.into(), Arc::new(image));
    }

    /// Register a decoded sprite sheet under its atlas image `src` key.
    pub fn insert_sprite(&mut self, image_src: impl Into<String>, sheet: SpriteSheet) {
        self.sprites.insert(image_src.into(), Arc::new(sheet));
    }

    /// Register a loaded font under its source `url` key.
    pub fn insert_font(&mut self, url: impl Into<String>, font: ezu_core::text::Font) {
        self.fonts.insert(url.into(), Arc::new(font));
    }
}

impl Default for BrushBankLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetLoader for BrushBankLoader {
    fn load(&self, name: &str) -> Result<Asset, AssetError> {
        let src = name;
        match parse_src_scheme(src)? {
            SrcScheme::Builtin(key) => {
                // Two-step lookup: bundled brushes register under bare
                // names, but a host-driven `bindSource` may insert
                // under the full `builtin:NAME` key. Try both.
                if let Some(b) = self.bank.get(key) {
                    return Ok(Asset::Brush(b.clone()));
                }
                if let Some(b) = self.bank.get(src) {
                    return Ok(Asset::Brush(b.clone()));
                }
                if let Some(s) = self.sprites.get(key).or_else(|| self.sprites.get(src)) {
                    return Ok(Asset::Sprite(s.clone()));
                }
                if let Some(f) = self.fonts.get(key).or_else(|| self.fonts.get(src)) {
                    return Ok(font_asset(f));
                }
                if let Some(img) = self.images.get(key) {
                    return Ok(Asset::Image(img.clone()));
                }
                if let Some(img) = self.images.get(src) {
                    return Ok(Asset::Image(img.clone()));
                }
                Err(AssetError::NotFound(src.to_string()))
            }
            SrcScheme::File(path) => {
                // Host may pre-populate the bank under the full
                // `file:PATH` key (wasm `bindSource`); try that first
                // so wasm hosts work without disk.
                if let Some(b) = self.bank.get(src) {
                    return Ok(Asset::Brush(b.clone()));
                }
                if let Some(s) = self.sprites.get(src) {
                    return Ok(Asset::Sprite(s.clone()));
                }
                if let Some(f) = self.fonts.get(src) {
                    return Ok(font_asset(f));
                }
                if let Some(img) = self.images.get(src) {
                    return Ok(Asset::Image(img.clone()));
                }
                if let Some(asset) = load_brush_file(self.brushes_dir.as_deref(), path, src)? {
                    return Ok(asset);
                }
                if let Some(asset) = load_font_file(path, src)? {
                    return Ok(asset);
                }
                if let Some(asset) = load_image_file(self.images_dir.as_deref(), path, src)? {
                    return Ok(asset);
                }
                Err(AssetError::NotFound(src.to_string()))
            }
            SrcScheme::Http(_) => {
                // Prefetched (CLI `prefetch_doc_assets`) or
                // host-supplied (`bindSource`) — same insertion key:
                // the full URL string.
                if let Some(b) = self.bank.get(src) {
                    return Ok(Asset::Brush(b.clone()));
                }
                if let Some(s) = self.sprites.get(src) {
                    return Ok(Asset::Sprite(s.clone()));
                }
                if let Some(f) = self.fonts.get(src) {
                    return Ok(font_asset(f));
                }
                if let Some(img) = self.images.get(src) {
                    return Ok(Asset::Image(img.clone()));
                }
                Err(AssetError::NotFound(src.to_string()))
            }
            SrcScheme::Data(_) => {
                // Decoded up front by `prefetch_doc_assets` into the bank; fall
                // back to decoding inline (no I/O) for hosts that don't prefetch
                // (wasm). Small inline assets, so a per-call decode is fine.
                if let Some(b) = self.bank.get(src) {
                    return Ok(Asset::Brush(b.clone()));
                }
                if let Some(f) = self.fonts.get(src) {
                    return Ok(font_asset(f));
                }
                if let Some(img) = self.images.get(src) {
                    return Ok(Asset::Image(img.clone()));
                }
                load_data_url(src)
            }
        }
    }
}

/// Parsed `src` URI scheme. Style `src` fields are required to carry an
/// explicit scheme so built-ins, disk paths, and URLs are unambiguous.
#[derive(Debug, Clone, Copy)]
enum SrcScheme<'a> {
    /// `builtin:NAME` — looked up in the loader's in-memory bank
    /// (bundled brushes + host-registered resources).
    Builtin(&'a str),
    /// `file:PATH` — disk path resolved against `brushes_dir` /
    /// `images_dir`; absolute paths are honoured as-is.
    File(&'a str),
    /// `http(s)://URL` — pre-fetched by the host and stored in the
    /// bank under the full URL string. The `_unused` payload mirrors
    /// the other variants for symmetry; the lookup uses the full
    /// `src` (matched against `bank` / `images` by the URL key).
    Http(#[allow(dead_code)] &'a str),
    /// `data:[<mediatype>][;base64],<payload>` — a self-contained inline
    /// asset. Decoded in-process (no I/O), so it works in every host
    /// including wasm.
    Data(#[allow(dead_code)] &'a str),
}

fn parse_src_scheme(src: &str) -> Result<SrcScheme<'_>, AssetError> {
    if let Some(rest) = src.strip_prefix("builtin:") {
        Ok(SrcScheme::Builtin(rest))
    } else if let Some(rest) = src.strip_prefix("file:") {
        Ok(SrcScheme::File(rest))
    } else if src.starts_with("http://") || src.starts_with("https://") {
        Ok(SrcScheme::Http(src))
    } else if src.starts_with("data:") {
        Ok(SrcScheme::Data(src))
    } else {
        Err(AssetError::Other(format!(
            "src `{src}` is missing a scheme — use `builtin:NAME`, `file:PATH`, `http(s)://URL`, or `data:`"
        )))
    }
}

/// A decoded `data:` URL — its media type (lowercased, e.g. `image/png`;
/// empty if unspecified) and raw payload bytes.
struct DataUrl {
    media_type: String,
    bytes: Vec<u8>,
}

/// Parse a `data:[<mediatype>][;base64],<payload>` URL into its media type
/// and decoded bytes. `;base64` payloads are base64-decoded; otherwise the
/// payload is percent-decoded (UTF-8 text — e.g. an inline `.myb` brush).
fn decode_data_url(src: &str) -> Result<DataUrl, AssetError> {
    let body = src
        .strip_prefix("data:")
        .ok_or_else(|| AssetError::Decode {
            src: src.to_string(),
            msg: "not a data URL".into(),
        })?;
    let (meta, payload) = body.split_once(',').ok_or_else(|| AssetError::Decode {
        src: src.to_string(),
        msg: "malformed data URL (missing `,`)".into(),
    })?;
    let is_base64 = meta.split(';').any(|s| s.eq_ignore_ascii_case("base64"));
    let media_type = meta.split(';').next().unwrap_or("").to_ascii_lowercase();
    let bytes = if is_base64 {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(payload.trim())
            .map_err(|e| AssetError::Decode {
                src: src.to_string(),
                msg: format!("base64: {e}"),
            })?
    } else {
        percent_decode(payload)
    };
    Ok(DataUrl { media_type, bytes })
}

/// Minimal `%XX` percent-decoding for non-base64 `data:` payloads. Invalid
/// escapes are passed through byte-for-byte.
fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Decode a `data:` URL into an [`Asset`]: `image/*` → an image, `font/*`
/// (or sniffed sfnt magic) → a font; anything else is tried as a `.myb`
/// brush, then (as a fallback for octet-stream/unspecified types) as a
/// sniffed image.
fn load_data_url(src: &str) -> Result<Asset, AssetError> {
    let data = decode_data_url(src)?;
    let as_image = |bytes: &[u8]| {
        decode_image_bytes(bytes)
            .map(|r| Asset::Image(Arc::new(r)))
            .map_err(|e| AssetError::Decode {
                src: src.to_string(),
                msg: e,
            })
    };
    if data.media_type.starts_with("image/") {
        return as_image(&data.bytes);
    }
    if data.media_type.starts_with("font/") || is_font_magic(&data.bytes) {
        let font = ezu_core::text::Font::from_bytes(Arc::from(data.bytes), 0).map_err(|e| {
            AssetError::Decode {
                src: src.to_string(),
                msg: e.to_string(),
            }
        })?;
        return Ok(Asset::Font(Arc::new(font) as OpaqueValue));
    }
    // Non-image media type: prefer a brush, else sniff as an image.
    if let Ok(text) = std::str::from_utf8(&data.bytes) {
        if let Ok(brush) = hokusai::myb::from_str(text) {
            return Ok(Asset::Brush(Arc::new(brush)));
        }
    }
    as_image(&data.bytes)
}

/// Wrap a bank font into its type-erased [`Asset`] form.
fn font_asset(font: &Arc<ezu_core::text::Font>) -> Asset {
    Asset::Font(font.clone() as OpaqueValue)
}

/// Whether `bytes` start with an sfnt font magic (TTF / OTF / TTC).
fn is_font_magic(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some([0x00, 0x01, 0x00, 0x00] | b"OTTO" | b"ttcf" | b"true")
    )
}

fn load_brush_file(
    dir: Option<&std::path::Path>,
    path: &str,
    src: &str,
) -> Result<Option<Asset>, AssetError> {
    // Absolute paths bypass the configured dir.
    let abs = std::path::Path::new(path);
    let candidates: Vec<std::path::PathBuf> = if abs.is_absolute() {
        vec![abs.to_path_buf()]
    } else {
        match dir {
            Some(d) => {
                let base = d.join(path);
                vec![base.clone(), base.with_extension("myb")]
            }
            None => return Ok(None),
        }
    };
    for path in &candidates {
        if !path.exists() || !is_brush_extension(path) {
            continue;
        }
        let bytes = std::fs::read_to_string(path).map_err(|e| AssetError::Decode {
            src: src.to_string(),
            msg: e.to_string(),
        })?;
        let brush = hokusai::myb::from_str(&bytes).map_err(|e| AssetError::Decode {
            src: src.to_string(),
            msg: e.to_string(),
        })?;
        return Ok(Some(Asset::Brush(Arc::new(brush))));
    }
    Ok(None)
}

fn load_image_file(
    dir: Option<&std::path::Path>,
    path: &str,
    src: &str,
) -> Result<Option<Asset>, AssetError> {
    let abs = std::path::Path::new(path);
    let candidates: Vec<std::path::PathBuf> = if abs.is_absolute() {
        vec![abs.to_path_buf()]
    } else {
        match dir {
            Some(d) => {
                let base = d.join(path);
                vec![
                    base.clone(),
                    base.with_extension("png"),
                    base.with_extension("webp"),
                ]
            }
            None => return Ok(None),
        }
    };
    for path in &candidates {
        if !path.exists() || is_brush_extension(path) {
            continue;
        }
        let raster = decode_image_file(path).map_err(|e| AssetError::Decode {
            src: src.to_string(),
            msg: e,
        })?;
        return Ok(Some(Asset::Image(Arc::new(raster))));
    }
    Ok(None)
}

fn is_brush_extension(path: &std::path::Path) -> bool {
    matches!(path.extension().and_then(|s| s.to_str()), Some("myb"))
}

/// Read a `file:` font at eval time. Only absolute paths resolve here
/// (there is no configured fonts dir); relative `file:` fonts are staged
/// up front by `prefetch_doc_assets`, which also honours a source's TTC
/// `index` — this lazy path always loads face 0.
fn load_font_file(path: &str, src: &str) -> Result<Option<Asset>, AssetError> {
    let path = std::path::Path::new(path);
    let is_font = matches!(
        path.extension().and_then(|s| s.to_str()),
        Some("ttf" | "otf" | "ttc")
    );
    if !is_font || !path.is_absolute() || !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(|e| AssetError::Decode {
        src: src.to_string(),
        msg: e.to_string(),
    })?;
    let font =
        ezu_core::text::Font::from_bytes(Arc::from(bytes), 0).map_err(|e| AssetError::Decode {
            src: src.to_string(),
            msg: e.to_string(),
        })?;
    Ok(Some(Asset::Font(Arc::new(font) as OpaqueValue)))
}

/// Per-render loader that overlays tile-scoped bindings on top of a
/// base [`AssetLoader`]. The host fills this with one entry per
/// host-supplied feature layer (or other tile-scoped asset) before
/// rendering a tile; document-scoped lookups fall through to the base.
///
/// Bindings are keyed by the exact name the style references — by
/// convention `<source>.<layer>` for per-tile MVT layers and bare
/// `<source>` for per-tile scalar fields (DEM).
pub struct TileLoader<'a> {
    base: &'a dyn AssetLoader,
    bindings: HashMap<String, Binding>,
    tile: TileId,
}

struct Binding {
    asset: Asset,
    /// Stable identity hash mixed into consuming nodes' cache keys.
    /// We hash `(tile, name)` rather than the payload contents — for
    /// per-tile data, tile id already uniquely identifies the binding.
    hash: u128,
}

impl<'a> TileLoader<'a> {
    pub fn new(base: &'a dyn AssetLoader, tile: TileId) -> Self {
        Self {
            base,
            bindings: HashMap::new(),
            tile,
        }
    }

    /// Bind a feature layer under `name`. By convention `name` is
    /// `"<source>.<layer>"`; the style's `features` node references
    /// the same `(source, layer)` pair.
    pub fn bind_features(&mut self, name: impl Into<String>, layer: FeatureLayer) -> &mut Self {
        let name = name.into();
        let hash = self.binding_hash(&name);
        let opaque: OpaqueValue = Arc::new(layer) as Arc<dyn Any + Send + Sync>;
        self.bindings.insert(
            name,
            Binding {
                asset: Asset::Features(opaque),
                hash,
            },
        );
        self
    }

    /// Bind a stitched per-tile RGBA raster under `name` (by
    /// convention the bare `<source>` name matching the style's
    /// `raster` sources entry). The buffer must be canvas-padded
    /// (`padded_size × padded_size`, premultiplied RGBA8) — the
    /// `raster` node passes it straight through as a `Raster` port
    /// value.
    pub fn bind_raster(&mut self, name: impl Into<String>, raster: RasterBuf) -> &mut Self {
        let name = name.into();
        let hash = self.binding_hash(&name);
        self.bindings.insert(
            name,
            Binding {
                asset: Asset::Image(Arc::new(raster)),
                hash,
            },
        );
        self
    }

    /// Bind a decoded scalar field under `name` (by convention the
    /// bare `<source>` name matching the style's DEM `sources` entry).
    /// For DEM data, populate `geo_scale` on the field so gradient
    /// consumers (`hillshade`, `slope`) produce real-world slopes.
    pub fn bind_scalar_field(&mut self, name: impl Into<String>, field: ScalarField) -> &mut Self {
        let name = name.into();
        let hash = self.binding_hash(&name);
        self.bindings.insert(
            name,
            Binding {
                asset: Asset::ScalarField(Arc::new(field)),
                hash,
            },
        );
        self
    }

    /// Bind every layer of a decoded MVT tile under
    /// `<source-name>.<layer-name>`. The style's `features` nodes
    /// reference the same `source` (matching one of the document's
    /// `sources` entries) plus a `layer` to look up these bindings.
    pub fn bind_mvt(&mut self, source: &str, tile: DecodedTile) -> &mut Self {
        for layer in tile.layers {
            let key = format!("{source}.{}", layer.name);
            self.bind_features(key, layer);
        }
        self
    }

    fn binding_hash(&self, name: &str) -> u128 {
        let mut h = Xxh3::new();
        h.update(&self.tile.z.to_le_bytes());
        h.update(&self.tile.x.to_le_bytes());
        h.update(&self.tile.y.to_le_bytes());
        h.update(name.as_bytes());
        h.digest128()
    }
}

impl AssetLoader for TileLoader<'_> {
    fn load(&self, name: &str) -> Result<Asset, AssetError> {
        if let Some(b) = self.bindings.get(name) {
            return Ok(b.asset.clone());
        }
        // Tile bindings (`<source>` or `<source>.<layer>`) never carry
        // a scheme; asset srcs (`builtin:`, `file:`, `http(s)://`) do.
        // An unbound name without a scheme means the source had no
        // such layer (or no MVT was bound for this tile at all) —
        // surface a clean `NotFound` so consumers like the `features`
        // op can fall back to an empty layer instead of bubbling a
        // "missing scheme" error from the URL/file loader.
        if !looks_like_asset_src(name) {
            return Err(AssetError::NotFound(name.to_string()));
        }
        self.base.load(name)
    }
    fn hash(&self, name: &str) -> u128 {
        if let Some(b) = self.bindings.get(name) {
            return b.hash;
        }
        if !looks_like_asset_src(name) {
            // Bindings absent → constant hash so cache keys are stable
            // across tiles that share the "missing" state.
            return 0;
        }
        self.base.hash(name)
    }
}

/// `true` iff `name` looks like a URL/file/builtin asset src (i.e.
/// has a `scheme:` prefix). Tile bindings never carry a scheme.
fn looks_like_asset_src(name: &str) -> bool {
    name.contains(':')
}

/// Decode a PNG (or other format supported by the `image` crate) into a
/// premultiplied-alpha RGBA8 [`RasterBuf`]. Returns a stringified error
/// on any decode failure.
fn decode_image_file(path: &std::path::Path) -> Result<RasterBuf, String> {
    let img = image::open(path).map_err(|e| e.to_string())?.to_rgba8();
    Ok(rgba_to_premul_raster(img))
}

/// Decode image bytes (PNG / WebP / anything `image` sniffs from the
/// content header) into a premultiplied-RGBA8 [`RasterBuf`]. The
/// twin of [`decode_image_file`] for callers that already have the
/// raw bytes in memory (e.g. an HTTP body).
pub fn decode_image_bytes(bytes: &[u8]) -> Result<RasterBuf, String> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    Ok(rgba_to_premul_raster(img))
}

/// Resolve a sprite source's `index` into a `name → rect` map.
///
/// An [`ezu_style::SpriteIndex::Inline`] map is converted directly; a
/// [`ezu_style::SpriteIndex::Url`] needs its already-fetched JSON text in
/// `fetched_json` (the host performs the I/O, keeping this pure). The JSON
/// uses the MapLibre sprite-index shape, so a fetched `sprite.json` and an
/// inline index deserialize identically.
pub fn build_sprite_icons(
    index: &ezu_style::SpriteIndex,
    fetched_json: Option<&str>,
) -> Result<HashMap<String, SpriteRect>, String> {
    let entries: HashMap<String, ezu_style::IconRect> = match index {
        ezu_style::SpriteIndex::Inline(map) => map.clone(),
        ezu_style::SpriteIndex::Url(_) => {
            let text = fetched_json.ok_or("sprite index URL was not fetched")?;
            serde_json::from_str(text).map_err(|e| format!("sprite index parse: {e}"))?
        }
    };
    Ok(entries
        .into_iter()
        .map(|(name, r)| {
            (
                name,
                SpriteRect {
                    x: r.x,
                    y: r.y,
                    width: r.width,
                    height: r.height,
                    pixel_ratio: r.pixel_ratio,
                },
            )
        })
        .collect())
}

fn rgba_to_premul_raster(img: image::RgbaImage) -> RasterBuf {
    let (w, h) = img.dimensions();
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for px in img.pixels() {
        let [r, g, b, a] = px.0;
        // Premultiply: ezu's RasterBuf carries straight RGBA *premul*.
        let af = a as f32 / 255.0;
        pixels.push((r as f32 * af).round() as u8);
        pixels.push((g as f32 * af).round() as u8);
        pixels.push((b as f32 * af).round() as u8);
        pixels.push(a);
    }
    RasterBuf {
        width: w,
        height: h,
        pixels,
    }
}

/// Trade-off setting for PNG encoding. `Default` matches the historical
/// behaviour (`tiny-skia` defaults, `miniz` mid-range deflate).
///
/// - `Fast` — smallest CPU cost, larger files. Good for live preview /
///   live-editor refresh paths where you redraw constantly.
/// - `Default` — balanced; the safe everywhere choice.
/// - `Best` — smallest files at a 2–4× CPU cost. Good for cached tile
///   pyramids that ship over the network.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PngCompression {
    Fast,
    #[default]
    Default,
    Best,
}

/// Crop a padded raster down to the central `tile_size` × `tile_size`
/// region and encode as PNG with the default compression preset.
pub fn raster_to_png(buf: &RasterBuf, tile_size: u32, pad: u32) -> Result<Vec<u8>, PaintError> {
    raster_to_png_with(buf, tile_size, pad, PngCompression::Default)
}

/// Like [`raster_to_png`] but lets the caller pick a compression
/// preset. The `Default` variant takes the original `tiny-skia` PNG
/// fast path (no extra demultiply); the other variants route through
/// `image`'s `PngEncoder` over the cropped, demultiplied RGBA buffer.
pub fn raster_to_png_with(
    buf: &RasterBuf,
    tile_size: u32,
    pad: u32,
    compression: PngCompression,
) -> Result<Vec<u8>, PaintError> {
    if matches!(compression, PngCompression::Default) {
        let padded = pixmap_from_raster(buf)?;
        if pad == 0 && padded.width() == tile_size && padded.height() == tile_size {
            return padded.encode_png().map_err(|_| PaintError::PngEncode);
        }
        let mut out = Pixmap::new(tile_size, tile_size).ok_or(PaintError::PngEncode)?;
        out.draw_pixmap(
            -(pad as i32),
            -(pad as i32),
            padded.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        return out.encode_png().map_err(|_| PaintError::PngEncode);
    }
    let rgba = raster_to_rgba8(buf, tile_size, pad);
    encode_rgba8_png(tile_size, tile_size, &rgba, compression)
}

fn encode_rgba8_png(
    width: u32,
    height: u32,
    straight_rgba: &[u8],
    compression: PngCompression,
) -> Result<Vec<u8>, PaintError> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    let ct = match compression {
        PngCompression::Fast => CompressionType::Fast,
        PngCompression::Default => CompressionType::Default,
        PngCompression::Best => CompressionType::Best,
    };
    let mut out = Vec::new();
    let encoder = PngEncoder::new_with_quality(&mut out, ct, FilterType::Adaptive);
    image::ImageEncoder::write_image(
        encoder,
        straight_rgba,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|_| PaintError::PngEncode)?;
    Ok(out)
}

fn pixmap_from_raster(buf: &RasterBuf) -> Result<Pixmap, PaintError> {
    let mut p = Pixmap::new(buf.width, buf.height).ok_or(PaintError::PngEncode)?;
    p.data_mut().copy_from_slice(&buf.pixels);
    Ok(p)
}

/// Crop a padded raster down to the central `tile_size` × `tile_size`
/// region and encode as lossless WebP via the pure-Rust `image-webp`
/// codec. WebP is typically 20–40 % smaller than the PNG output for
/// the same painterly tile while staying lossless, so it's the better
/// default for cached tile pyramids.
pub fn raster_to_webp(buf: &RasterBuf, tile_size: u32, pad: u32) -> Result<Vec<u8>, PaintError> {
    // The pure-Rust WebP encoder wants straight (un-premul) RGBA, which
    // `raster_to_rgba8` already produces alongside the crop.
    let rgba = raster_to_rgba8(buf, tile_size, pad);
    encode_rgba8_webp(tile_size, tile_size, &rgba)
}

/// Encode a tiny-skia `Pixmap` (premultiplied RGBA8) as lossless WebP.
/// Demultiplies in place and hands the straight-RGBA buffer to the
/// pure-Rust WebP encoder. Use this for outputs that aren't tile-sized
/// (e.g. the `ezu-cli bbox` mosaic).
pub fn pixmap_to_webp(pixmap: &Pixmap) -> Result<Vec<u8>, PaintError> {
    let (w, h) = (pixmap.width(), pixmap.height());
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for p in pixmap.pixels() {
        let p = p.demultiply();
        rgba.extend_from_slice(&[p.red(), p.green(), p.blue(), p.alpha()]);
    }
    encode_rgba8_webp(w, h, &rgba)
}

fn encode_rgba8_webp(width: u32, height: u32, straight_rgba: &[u8]) -> Result<Vec<u8>, PaintError> {
    let mut out = Vec::new();
    let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
    image::ImageEncoder::write_image(
        encoder,
        straight_rgba,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|e| PaintError::WebpEncode(e.to_string()))?;
    Ok(out)
}

/// Crop a padded raster down to the central tile region and return
/// straight (un-premultiplied) 8-bit RGBA bytes — directly compatible
/// with `new ImageData(new Uint8ClampedArray(...), w, h)` in JS.
pub fn raster_to_rgba8(buf: &RasterBuf, tile_size: u32, pad: u32) -> Vec<u8> {
    let padded = match pixmap_from_raster(buf) {
        Ok(p) => p,
        Err(_) => return vec![0; (tile_size * tile_size * 4) as usize],
    };
    let tile_pixmap = if pad == 0 && padded.width() == tile_size && padded.height() == tile_size {
        padded
    } else {
        let mut out = match Pixmap::new(tile_size, tile_size) {
            Some(p) => p,
            None => return vec![0; (tile_size * tile_size * 4) as usize],
        };
        out.draw_pixmap(
            -(pad as i32),
            -(pad as i32),
            padded.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        out
    };
    let mut rgba = Vec::with_capacity((tile_size * tile_size * 4) as usize);
    for p in tile_pixmap.pixels() {
        let p = p.demultiply();
        rgba.extend_from_slice(&[p.red(), p.green(), p.blue(), p.alpha()]);
    }
    rgba
}

// ---------------------------------------------------------------------------
// URL-aware asset prefetch (feature `http`)

/// Walk a [`Document`]'s `assets` block and stage every entry into the
/// given [`BrushBankLoader`]: brushes (`hokusai::Brush`), images
/// (`RasterBuf`). Each entry's `src` may be a plain file path resolved
/// against `base_dir` or an `http(s)://` URL fetched via `reqwest`.
/// Tile-scoped variants (`mvt`, `pmtiles`, `dem`) are skipped — those
/// are fetched per-render by the CLI's source registries.
///
/// Available with the `http` feature; off on `wasm32` since the JS
/// host handles fetching there.
#[cfg(feature = "http")]
pub async fn prefetch_doc_assets(
    doc: &ezu_style::Document,
    base_dir: &std::path::Path,
    loader: &mut BrushBankLoader,
) -> Result<(), String> {
    // Keys must match what the source/paint factories actually pass to
    // `AssetLoader::load` at eval time. `brush-file` and `image`
    // resolve `@source-name` → that source's `src` string, so the bank
    // lookup key is `decl.src` (not the source name).
    for (name, decl) in &doc.sources {
        let _ = base_dir; // file:-scheme srcs resolve at load() time
        match decl {
            ezu_style::SourceDecl::Brush(file) => {
                // Only HTTP brushes need pre-fetching — `builtin:` is
                // already in the bank, `file:` reads at eval time.
                if !is_http_url(&file.src) {
                    continue;
                }
                if loader.bank.contains_key(&file.src) {
                    continue;
                }
                let json = http_text(&file.src)
                    .await
                    .map_err(|e| format!("brush `{name}`: {e}"))?;
                let brush = hokusai::myb::from_str(&json)
                    .map_err(|e| format!("brush `{name}` parse: {e}"))?;
                loader.insert(file.src.clone(), brush);
            }
            ezu_style::SourceDecl::Image(file) => {
                if !is_http_url(&file.src) {
                    continue;
                }
                if loader.images.contains_key(&file.src) {
                    continue;
                }
                let bytes = http_bytes(&file.src)
                    .await
                    .map_err(|e| format!("image `{name}`: {e}"))?;
                let raster = decode_image_bytes(&bytes)
                    .map_err(|e| format!("image `{name}` decode: {e}"))?;
                loader.insert_image(file.src.clone(), raster);
            }
            ezu_style::SourceDecl::Sprite(sprite) => {
                // Sprites have no lazy eval-time path (the loader can't fetch),
                // so build the whole sheet up front for both http and file.
                if loader.sprites.contains_key(&sprite.image) {
                    continue;
                }
                let atlas_bytes = read_asset_bytes(&sprite.image, base_dir)
                    .await
                    .map_err(|e| format!("sprite `{name}` atlas: {e}"))?;
                let atlas = decode_image_bytes(&atlas_bytes)
                    .map_err(|e| format!("sprite `{name}` atlas decode: {e}"))?;
                let fetched = match &sprite.index {
                    ezu_style::SpriteIndex::Url(u) => Some(
                        read_asset_text(u, base_dir)
                            .await
                            .map_err(|e| format!("sprite `{name}` index: {e}"))?,
                    ),
                    ezu_style::SpriteIndex::Inline(_) => None,
                };
                let icons = build_sprite_icons(&sprite.index, fetched.as_deref())
                    .map_err(|e| format!("sprite `{name}`: {e}"))?;
                loader.insert_sprite(sprite.image.clone(), SpriteSheet { atlas, icons });
            }
            ezu_style::SourceDecl::Font(font) => {
                // Staged up front for every scheme so a TTC `index` is
                // honoured and relative `file:` paths resolve against
                // `base_dir` (the lazy eval-time path handles neither).
                if loader.fonts.contains_key(&font.url) {
                    continue;
                }
                let bytes = read_asset_bytes(&font.url, base_dir)
                    .await
                    .map_err(|e| format!("font `{name}`: {e}"))?;
                let face = ezu_core::text::Font::from_bytes(Arc::from(bytes), font.index)
                    .map_err(|e| format!("font `{name}`: {e}"))?;
                loader.fonts.insert(font.url.clone(), Arc::new(face));
            }
            // Tile-scoped — handled per-render elsewhere. GeoJSON is
            // projected + bound per tile by the host driver, not here.
            ezu_style::SourceDecl::Mvt(_)
            | ezu_style::SourceDecl::Pmtiles(_)
            | ezu_style::SourceDecl::Dem(_)
            | ezu_style::SourceDecl::GeoJson(_)
            | ezu_style::SourceDecl::Raster(_) => {}
        }
    }
    Ok(())
}

/// Read an asset src (`http(s)://` or `file:PATH`, the latter resolved
/// against `base_dir`) into bytes.
#[cfg(feature = "http")]
async fn read_asset_bytes(src: &str, base_dir: &std::path::Path) -> Result<Vec<u8>, String> {
    if is_http_url(src) {
        http_bytes(src).await
    } else if src.starts_with("data:") {
        decode_data_url(src)
            .map(|d| d.bytes)
            .map_err(|e| e.to_string())
    } else if let Some(path) = src.strip_prefix("file:") {
        std::fs::read(resolve_file(path, base_dir)).map_err(|e| e.to_string())
    } else {
        Err(format!(
            "unsupported src `{src}` — use `http(s)://URL`, `file:PATH`, or `data:`"
        ))
    }
}

/// Text counterpart of [`read_asset_bytes`].
#[cfg(feature = "http")]
async fn read_asset_text(src: &str, base_dir: &std::path::Path) -> Result<String, String> {
    if is_http_url(src) {
        http_text(src).await
    } else if src.starts_with("data:") {
        let d = decode_data_url(src).map_err(|e| e.to_string())?;
        String::from_utf8(d.bytes).map_err(|e| e.to_string())
    } else if let Some(path) = src.strip_prefix("file:") {
        std::fs::read_to_string(resolve_file(path, base_dir)).map_err(|e| e.to_string())
    } else {
        Err(format!(
            "unsupported src `{src}` — use `http(s)://URL`, `file:PATH`, or `data:`"
        ))
    }
}

#[cfg(feature = "http")]
fn resolve_file(path: &str, base_dir: &std::path::Path) -> std::path::PathBuf {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    }
}

#[cfg(feature = "http")]
async fn http_text(url: &str) -> Result<String, String> {
    reqwest::get(url)
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())
}

#[cfg(feature = "http")]
async fn http_bytes(url: &str) -> Result<Vec<u8>, String> {
    Ok(reqwest::get(url)
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?
        .to_vec())
}

#[cfg(feature = "http")]
fn is_http_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    /// Encode a 2×1 red/green PNG and wrap it as a base64 `data:` URL.
    fn red_green_png_data_url() -> String {
        let mut img = image::RgbaImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.put_pixel(1, 0, image::Rgba([0, 255, 0, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
        format!("data:image/png;base64,{b64}")
    }

    #[test]
    fn data_url_image_loads_through_the_asset_loader() {
        let src = red_green_png_data_url();
        let loader = BrushBankLoader::empty();
        match loader.load(&src).expect("data url loads") {
            Asset::Image(img) => {
                assert_eq!((img.width, img.height), (2, 1));
                // Red then green, premultiplied-opaque round-trips unchanged.
                assert_eq!(img.pixel(0, 0), [255, 0, 0, 255]);
                assert_eq!(img.pixel(1, 0), [0, 255, 0, 255]);
            }
            _ => panic!("expected an Image asset from a data:image/png URL"),
        }
    }

    #[test]
    fn file_scheme_font_loads_through_the_asset_loader() {
        // Absolute `file:` path to the ezu-core test font (a Noto Sans
        // subset vendored for the `text` feature tests).
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
        let src = format!("file:{}", path.display());
        let loader = BrushBankLoader::empty();
        match loader.load(&src).expect("font loads") {
            Asset::Font(opq) => {
                let font = opq
                    .downcast::<ezu_core::text::Font>()
                    .expect("payload is ezu_core::text::Font");
                assert!(font.covers('A'));
                assert!(!font.covers('0')); // digits live in the other subset
            }
            other => panic!("expected a Font asset, got {other:?}"),
        }
    }

    #[test]
    fn font_magic_is_sniffed_for_data_urls() {
        assert!(is_font_magic(&[0x00, 0x01, 0x00, 0x00, 0xff]));
        assert!(is_font_magic(b"OTTO...."));
        assert!(is_font_magic(b"ttcf...."));
        assert!(!is_font_magic(b"\x89PNG\r\n"));
        assert!(!is_font_magic(b"{}"));
    }

    #[test]
    fn data_url_percent_decoding_and_media_type() {
        // Non-base64, percent-encoded text payload.
        let d = decode_data_url("data:text/plain,a%20b%2Fc").unwrap();
        assert_eq!(d.media_type, "text/plain");
        assert_eq!(d.bytes, b"a b/c");

        // Media type is lowercased; `;base64` is detected case-insensitively.
        let d = decode_data_url("data:image/PNG;Base64,QUJD").unwrap();
        assert_eq!(d.media_type, "image/png");
        assert_eq!(d.bytes, b"ABC");

        // Malformed (no comma) is rejected.
        assert!(decode_data_url("data:image/png;base64").is_err());
    }
}
