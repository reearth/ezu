//! Host-side glue for rendering: pluggable `AssetLoader` impls and
//! conversion helpers between `ezu_graph::RasterBuf` and `tiny-skia` /
//! PNG output.

use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ezu_features::{mvt::DecodedTile, FeatureLayer};
use ezu_graph::{Asset, AssetError, AssetLoader, OpaqueValue, RasterBuf, TileId};
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
        // Document-scoped names with a leading `@` are accepted for
        // backwards compatibility with the original brush bank style.
        let key = src.strip_prefix('@').unwrap_or(src);
        if let Some(b) = self.bank.get(key) {
            return Ok(Asset::Brush(b.clone()));
        }
        if let Some(dir) = &self.brushes_dir {
            // Try `<dir>/<key>` then `<dir>/<key>.myb`.
            let candidates = [dir.join(key), dir.join(format!("{key}.myb"))];
            for path in &candidates {
                if path.exists() {
                    let bytes = std::fs::read_to_string(path).map_err(|e| AssetError::Decode {
                        src: src.to_string(),
                        msg: e.to_string(),
                    })?;
                    let brush = hokusai::myb::from_str(&bytes).map_err(|e| AssetError::Decode {
                        src: src.to_string(),
                        msg: e.to_string(),
                    })?;
                    return Ok(Asset::Brush(Arc::new(brush)));
                }
            }
        }
        if let Some(img) = self.images.get(key) {
            return Ok(Asset::Image(img.clone()));
        }
        if let Some(dir) = &self.images_dir {
            // Try `<dir>/<key>` (exact), then a small list of supported
            // image extensions. `image::open` sniffs by extension, so
            // both PNG and WebP flow through the same decode path.
            let candidates = [
                dir.join(key),
                dir.join(format!("{key}.png")),
                dir.join(format!("{key}.webp")),
            ];
            for path in &candidates {
                if path.exists() {
                    let raster = decode_image_file(path).map_err(|e| AssetError::Decode {
                        src: src.to_string(),
                        msg: e,
                    })?;
                    return Ok(Asset::Image(Arc::new(raster)));
                }
            }
        }
        Err(AssetError::NotFound(src.to_string()))
    }
}

/// Per-render loader that overlays tile-scoped bindings on top of a
/// base [`AssetLoader`]. The host fills this with one entry per
/// host-supplied feature layer (or other tile-scoped asset) before
/// rendering a tile; document-scoped lookups fall through to the base.
///
/// Bindings are keyed by the exact name the style references — by
/// convention `tile.<layer>` for per-tile features.
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
    /// `"tile.<layer>"`; the style's `features` node references it by
    /// the same string.
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

    /// Bind every layer of a decoded MVT tile under `tile.<layer-name>`.
    /// Convenience for hosts that decode MVT bytes per tile.
    pub fn bind_mvt(&mut self, tile: DecodedTile) -> &mut Self {
        for layer in tile.layers {
            let key = format!("tile.{}", layer.name);
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
        self.base.load(name)
    }
    fn hash(&self, name: &str) -> u128 {
        if let Some(b) = self.bindings.get(name) {
            return b.hash;
        }
        self.base.hash(name)
    }
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

/// Crop a padded raster down to the central `tile_size` × `tile_size`
/// region and encode as PNG.
pub fn raster_to_png(buf: &RasterBuf, tile_size: u32, pad: u32) -> Result<Vec<u8>, PaintError> {
    // Wrap our RGBA8 premul buffer as a tiny-skia Pixmap by copying. We
    // could share memory but the API requires a Pixmap-owned allocation.
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
    out.encode_png().map_err(|_| PaintError::PngEncode)
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
/// `gradient` assets are skipped (not yet supported here).
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
    // resolve `@asset` → that asset's `src` string, so the bank lookup
    // key is `decl.src` (not the asset name).
    for (name, decl) in &doc.assets {
        match decl.kind {
            ezu_style::AssetKind::Brush => {
                // Already pre-registered (built-in brush, or staged
                // earlier by the host) — no fetch needed.
                if loader.bank.contains_key(&decl.src) {
                    continue;
                }
                let json = fetch_asset_text(&decl.src, base_dir, "myb")
                    .await
                    .map_err(|e| format!("brush `{name}`: {e}"))?;
                let brush = hokusai::myb::from_str(&json)
                    .map_err(|e| format!("brush `{name}` parse: {e}"))?;
                loader.insert(decl.src.clone(), brush);
            }
            ezu_style::AssetKind::Image | ezu_style::AssetKind::MaskImage => {
                if loader.images.contains_key(&decl.src) {
                    continue;
                }
                let bytes = fetch_asset_bytes(&decl.src, base_dir, "png")
                    .await
                    .map_err(|e| format!("image `{name}`: {e}"))?;
                let raster = decode_image_bytes(&bytes)
                    .map_err(|e| format!("image `{name}` decode: {e}"))?;
                loader.insert_image(decl.src.clone(), raster);
            }
            ezu_style::AssetKind::Gradient => {}
        }
    }
    Ok(())
}

#[cfg(feature = "http")]
async fn fetch_asset_text(
    src: &str,
    base_dir: &std::path::Path,
    default_ext: &str,
) -> Result<String, String> {
    if is_http_url(src) {
        Ok(reqwest::get(src)
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())?)
    } else {
        let path = resolve_with_ext(base_dir, src, default_ext)
            .ok_or_else(|| format!("no file at {}", base_dir.join(src).display()))?;
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))
    }
}

#[cfg(feature = "http")]
async fn fetch_asset_bytes(
    src: &str,
    base_dir: &std::path::Path,
    default_ext: &str,
) -> Result<Vec<u8>, String> {
    if is_http_url(src) {
        Ok(reqwest::get(src)
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .bytes()
            .await
            .map_err(|e| e.to_string())?
            .to_vec())
    } else {
        let path = resolve_with_ext(base_dir, src, default_ext)
            .ok_or_else(|| format!("no file at {}", base_dir.join(src).display()))?;
        std::fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()))
    }
}

#[cfg(feature = "http")]
fn is_http_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

#[cfg(feature = "http")]
fn resolve_with_ext(base: &std::path::Path, src: &str, ext: &str) -> Option<std::path::PathBuf> {
    let direct = base.join(src);
    if direct.exists() {
        return Some(direct);
    }
    let with_ext = base.join(format!("{src}.{ext}"));
    with_ext.exists().then_some(with_ext)
}
