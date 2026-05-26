//! Host-side glue for rendering: pluggable `AssetLoader` impls and
//! conversion helpers between `ezu_graph::RasterBuf` and `tiny-skia` /
//! PNG output.

pub mod dem_decode;
pub use dem_decode::{decode_dem_tile, stitch_padded_field, DemDecodeError, DemTile};

#[cfg(feature = "http")]
pub mod dem;
#[cfg(feature = "http")]
pub use dem::{bind_dem_sources, build_dem_sources, DemFetchError, DemSourceRegistry};

use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ezu_features::{mvt::DecodedTile, FeatureLayer};
use ezu_graph::{Asset, AssetError, AssetLoader, OpaqueValue, RasterBuf, ScalarField, TileId};
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
                if let Some(img) = self.images.get(src) {
                    return Ok(Asset::Image(img.clone()));
                }
                if let Some(asset) = load_brush_file(self.brushes_dir.as_deref(), path, src)? {
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
                if let Some(img) = self.images.get(src) {
                    return Ok(Asset::Image(img.clone()));
                }
                Err(AssetError::NotFound(src.to_string()))
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
}

fn parse_src_scheme(src: &str) -> Result<SrcScheme<'_>, AssetError> {
    if let Some(rest) = src.strip_prefix("builtin:") {
        Ok(SrcScheme::Builtin(rest))
    } else if let Some(rest) = src.strip_prefix("file:") {
        Ok(SrcScheme::File(rest))
    } else if src.starts_with("http://") || src.starts_with("https://") {
        Ok(SrcScheme::Http(src))
    } else {
        Err(AssetError::Other(format!(
            "src `{src}` is missing a scheme — use `builtin:NAME`, `file:PATH`, or `http(s)://URL`"
        )))
    }
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
            // Tile-scoped — handled per-render elsewhere.
            ezu_style::SourceDecl::Mvt(_)
            | ezu_style::SourceDecl::Pmtiles(_)
            | ezu_style::SourceDecl::Dem(_) => {}
        }
    }
    Ok(())
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
