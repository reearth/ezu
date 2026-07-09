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
use std::sync::{Arc, RwLock};

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
/// A new loader starts empty. Populate it by inserting assets directly,
/// pointing it at a directory, or staging a document's declared sources
/// with [`prefetch_doc_assets`].
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
    /// SDF glyph stacks keyed by their `glyphs` source's
    /// [`asset_key`](ezu_style::GlyphsSource::asset_key) (the URL
    /// template with `{fontstack}` substituted, `{range}` kept).
    /// Interior-mutable so [`AssetLoader::load`] can create a stack
    /// lazily on first use; each stack's *ranges* then grow lazily as
    /// tiles demand codepoints (see [`AssetLoader::hash`] below for how
    /// caches stay correct while they grow).
    pub glyphs: RwLock<HashMap<String, Arc<ezu_core::text::SdfFontStack>>>,
}

impl BrushBankLoader {
    /// New loader with an empty bank. Register assets with the `insert*`
    /// methods, point it at a directory with [`with_dir`](Self::with_dir) /
    /// [`with_images_dir`](Self::with_images_dir), or stage a document's
    /// declared sources with [`prefetch_doc_assets`].
    pub fn new() -> Self {
        Self {
            bank: HashMap::new(),
            brushes_dir: None,
            images: HashMap::new(),
            images_dir: None,
            sprites: HashMap::new(),
            fonts: HashMap::new(),
            glyphs: RwLock::new(HashMap::new()),
        }
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

    /// Register an SDF glyph stack under its source's asset key
    /// ([`ezu_style::GlyphsSource::asset_key`]).
    pub fn insert_glyphs(&self, key: impl Into<String>, stack: Arc<ezu_core::text::SdfFontStack>) {
        self.glyphs
            .write()
            .expect("glyphs bank poisoned")
            .insert(key.into(), stack);
    }

    /// The stack registered under `key`, creating one lazily when the
    /// key is a glyphs URL template: `file:` templates read ranges from
    /// disk on demand (absolute paths — relative ones are staged by
    /// `prefetch_doc_assets`), `http(s)://` templates fetch blocking
    /// (feature `http`). Without any fetch path (a wasm host) the stack
    /// starts empty and every needed range must have been pushed via
    /// [`insert_glyphs`](Self::insert_glyphs) + `SdfFontStack::insert_range`
    /// up front; a warning is logged so nothing goes missing silently.
    pub fn glyphs_stack(&self, key: &str) -> Arc<ezu_core::text::SdfFontStack> {
        if let Some(stack) = self.glyphs.read().expect("glyphs bank poisoned").get(key) {
            return stack.clone();
        }
        let stack = Arc::new(match make_range_fetcher(key, None) {
            Some(fetcher) => ezu_core::text::SdfFontStack::with_fetcher(fetcher),
            None => {
                tracing::warn!(
                    "glyphs source `{key}`: this host cannot fetch ranges — bind every \
                     needed range up front or labels will drop their glyphs"
                );
                ezu_core::text::SdfFontStack::new()
            }
        });
        self.glyphs
            .write()
            .expect("glyphs bank poisoned")
            .entry(key.to_string())
            .or_insert(stack)
            .clone()
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
        // A `{range}` placeholder marks a glyphs URL template (a
        // `glyphs` source's asset key) — ranges resolve lazily inside
        // the stack, so the template itself is the loadable asset.
        if src.contains("{range}") {
            parse_src_scheme(src)?; // reject scheme-less templates
            return Ok(Asset::Glyphs(self.glyphs_stack(src) as OpaqueValue));
        }
        match parse_src_scheme(src)? {
            SrcScheme::Builtin(key) => {
                // Resolve against the in-memory bank the host populated.
                // A host may key an entry by the bare name or by the full
                // `builtin:NAME` src (e.g. a wasm `bindSource`), so try both.
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
                Err(AssetError::Other(format!(
                    "asset `{src}` is not registered in the in-memory bank. There are no \
                     bundled brushes — declare the asset in `sources` with a `file:`, \
                     `http(s):`, or `data:` `src`, or register it on the host before rendering."
                )))
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
            SrcScheme::System(spec) => {
                // A host may pre-populate the bank under the full
                // `system:…` key (CLI `prefetch_doc_assets`, or a wasm
                // `bindSource` supplying the bytes directly); use that
                // first so no re-scan/parse happens per load.
                if let Some(f) = self.fonts.get(src) {
                    return Ok(font_asset(f));
                }
                let query = parse_system_font(spec)?;
                Ok(Asset::Font(
                    Arc::new(load_system_font(&query)?) as OpaqueValue
                ))
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

    /// Glyph stacks are the one lazily *growing* asset: ranges accrete
    /// as tiles demand codepoints. Digest the loaded/failed range set
    /// so consumers' cache keys track exactly what affects output.
    ///
    /// The evaluator samples this hash *before* eval, so an eval that
    /// pulls a new range mid-flight stores its result under the
    /// pre-fetch key. That entry is stale but unreachable: ranges only
    /// grow (failures are remembered per range), so the digest never
    /// returns to a previous value — the next render keys on the grown
    /// set, misses, and re-evaluates. No stale hit is possible.
    fn hash(&self, name: &str) -> u128 {
        self.glyphs
            .read()
            .expect("glyphs bank poisoned")
            .get(name)
            .map(|stack| stack.ranges_hash())
            .unwrap_or(0)
    }
}

/// Build the lazy range fetcher for a glyphs URL template, if this host
/// can fetch at all: `file:` reads from disk (relative paths resolve
/// against `base_dir` when given, else must be absolute), `http(s)://`
/// fetches blocking (feature `http`). `None` → the host must push
/// ranges up front.
fn make_range_fetcher(
    template: &str,
    base_dir: Option<PathBuf>,
) -> Option<ezu_core::text::RangeFetcher> {
    if let Some(path_template) = template.strip_prefix("file:") {
        let path_template = path_template.to_string();
        return Some(Box::new(move |start, end| {
            let raw = path_template.replace("{range}", &format!("{start}-{end}"));
            let p = std::path::Path::new(&raw);
            let path = if p.is_absolute() {
                p.to_path_buf()
            } else if let Some(dir) = &base_dir {
                dir.join(p)
            } else {
                return Err(format!(
                    "relative glyphs path `{raw}` resolves only through prefetch (no base dir)"
                ));
            };
            std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
        }));
    }
    #[cfg(feature = "http")]
    if template.starts_with("http://") || template.starts_with("https://") {
        let template = template.to_string();
        return Some(Box::new(move |start, end| {
            http_bytes_blocking(template.replace("{range}", &format!("{start}-{end}")))
        }));
    }
    None
}

/// Fetch `url` synchronously on a throwaway thread. Range fetches run
/// inside `Node::eval`, which the CLI/server drive from a tokio
/// runtime; `reqwest::blocking` must not run on a runtime worker, so
/// isolate it. Ranges are small and cached forever in the stack, so
/// the per-fetch thread cost is negligible.
#[cfg(feature = "http")]
fn http_bytes_blocking(url: String) -> Result<Vec<u8>, String> {
    std::thread::spawn(move || -> Result<Vec<u8>, String> {
        Ok(reqwest::blocking::get(&url)
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .bytes()
            .map_err(|e| e.to_string())?
            .to_vec())
    })
    .join()
    .map_err(|_| "glyph fetch thread panicked".to_string())?
}

/// Parsed `src` URI scheme. Style `src` fields are required to carry an
/// explicit scheme so built-ins, disk paths, and URLs are unambiguous.
#[derive(Debug, Clone, Copy)]
enum SrcScheme<'a> {
    /// `builtin:NAME` — looked up in the loader's in-memory bank, which
    /// the host populates at runtime (e.g. a wasm `bindSource`). Nothing
    /// is bundled into the library, so an unregistered `NAME` is an error.
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
    /// `system:<family>[?weight=…&style=…]` — a font resolved by family
    /// name from the OS-installed font database (see
    /// [`parse_system_font`]). The payload is the part after `system:`.
    System(&'a str),
}

fn parse_src_scheme(src: &str) -> Result<SrcScheme<'_>, AssetError> {
    if let Some(rest) = src.strip_prefix("builtin:") {
        Ok(SrcScheme::Builtin(rest))
    } else if let Some(rest) = src.strip_prefix("file:") {
        Ok(SrcScheme::File(rest))
    } else if let Some(rest) = src.strip_prefix("system:") {
        Ok(SrcScheme::System(rest))
    } else if src.starts_with("http://") || src.starts_with("https://") {
        Ok(SrcScheme::Http(src))
    } else if src.starts_with("data:") {
        Ok(SrcScheme::Data(src))
    } else {
        Err(AssetError::Other(format!(
            "src `{src}` is missing a scheme — use `builtin:NAME`, `file:PATH`, `system:FAMILY`, `http(s)://URL`, or `data:`"
        )))
    }
}

/// A parsed `system:` font reference — a family name plus an optional
/// CSS weight/style to disambiguate among the faces of that family.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SystemFontQuery {
    family: String,
    /// CSS numeric weight, `100..=900` (default `400`).
    weight: u16,
    style: SystemFontStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemFontStyle {
    Normal,
    Italic,
    Oblique,
}

/// Parse the body of a `system:<family>[?weight=…&style=…]` font URI —
/// the part *after* the `system:` scheme prefix.
///
/// Grammar:
///
/// ```text
/// <family>[?<param>&<param>…]
/// <param> := weight=<100..=900> | style=<normal|italic|oblique>
/// ```
///
/// - `<family>` is the CSS family name (e.g. `Arial Unicode MS`). It may
///   be written literally with spaces, or percent-encoded
///   (`Arial%20Unicode%20MS`) — both decode to the same name, so a name
///   with spaces is natural to write. Only `%XX` escapes are decoded;
///   `+` is a literal.
/// - `weight` (optional, default `400`) is a CSS numeric weight, an
///   integer in `100..=900`.
/// - `style` (optional, default `normal`) is `normal`, `italic`, or
///   `oblique`, matched case-insensitively.
///
/// Query keys other than `weight`/`style`, a malformed key=value pair,
/// an out-of-range or non-numeric weight, an unknown style, or an empty
/// family are all rejected with a descriptive error.
///
/// Examples:
///
/// ```text
/// system:Arial Unicode MS
/// system:Helvetica?weight=700
/// system:Noto%20Sans?weight=300&style=italic
/// ```
fn parse_system_font(spec: &str) -> Result<SystemFontQuery, AssetError> {
    let (family_raw, query) = match spec.split_once('?') {
        Some((f, q)) => (f, Some(q)),
        None => (spec, None),
    };
    let family = String::from_utf8_lossy(&percent_decode(family_raw))
        .trim()
        .to_string();
    if family.is_empty() {
        return Err(AssetError::Other(format!(
            "system font `{spec}`: empty family name — use `system:FAMILY`"
        )));
    }
    let mut weight = 400u16;
    let mut style = SystemFontStyle::Normal;
    if let Some(query) = query {
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').ok_or_else(|| {
                AssetError::Other(format!(
                    "system font `{spec}`: malformed query part `{pair}` (expected `key=value`)"
                ))
            })?;
            match key {
                "weight" => {
                    let w: u16 = value.parse().map_err(|_| {
                        AssetError::Other(format!(
                            "system font `{spec}`: invalid weight `{value}` (expected an integer in 100..=900)"
                        ))
                    })?;
                    if !(100..=900).contains(&w) {
                        return Err(AssetError::Other(format!(
                            "system font `{spec}`: weight `{w}` out of range (expected 100..=900)"
                        )));
                    }
                    weight = w;
                }
                "style" => {
                    style = match value.to_ascii_lowercase().as_str() {
                        "normal" => SystemFontStyle::Normal,
                        "italic" => SystemFontStyle::Italic,
                        "oblique" => SystemFontStyle::Oblique,
                        other => {
                            return Err(AssetError::Other(format!(
                                "system font `{spec}`: unknown style `{other}` (expected normal, italic, or oblique)"
                            )))
                        }
                    };
                }
                other => {
                    return Err(AssetError::Other(format!(
                        "system font `{spec}`: unknown query key `{other}` (expected `weight` or `style`)"
                    )))
                }
            }
        }
    }
    Ok(SystemFontQuery {
        family,
        weight,
        style,
    })
}

/// Process-wide OS font database, scanned once on first `system:` font
/// resolution. Not built on wasm (no OS font database in a browser).
#[cfg(not(target_arch = "wasm32"))]
static SYSTEM_FONTS: std::sync::LazyLock<fontdb::Database> = std::sync::LazyLock::new(|| {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    db
});

/// Resolve a `system:` font query against the installed fonts, returning
/// the matched face as an [`ezu_core::text::Font`].
///
/// `fontdb` reports the face's index within its (possibly `.ttc`) file
/// alongside the bytes; that index is threaded into
/// [`ezu_core::text::Font::from_bytes`] so a collection's requested face
/// is the one parsed — mirroring how a `file:`/`http` source honours its
/// declared TTC `index`.
#[cfg(not(target_arch = "wasm32"))]
fn load_system_font(query: &SystemFontQuery) -> Result<ezu_core::text::Font, AssetError> {
    resolve_system_font(&SYSTEM_FONTS, query)
}

/// [`load_system_font`] against an explicit database — the seam the unit
/// tests use to exercise the query→bytes→face path over a fixture font,
/// independent of whatever fonts the host OS happens to have installed.
#[cfg(not(target_arch = "wasm32"))]
fn resolve_system_font(
    db: &fontdb::Database,
    query: &SystemFontQuery,
) -> Result<ezu_core::text::Font, AssetError> {
    let style = match query.style {
        SystemFontStyle::Normal => fontdb::Style::Normal,
        SystemFontStyle::Italic => fontdb::Style::Italic,
        SystemFontStyle::Oblique => fontdb::Style::Oblique,
    };
    let id = db
        .query(&fontdb::Query {
            families: &[fontdb::Family::Name(&query.family)],
            weight: fontdb::Weight(query.weight),
            stretch: fontdb::Stretch::Normal,
            style,
        })
        .ok_or_else(|| {
            AssetError::Other(format!("system font family '{}' not found", query.family))
        })?;
    let parsed = db
        .with_face_data(id, |data, face_index| {
            ezu_core::text::Font::from_bytes(Arc::from(data.to_vec()), face_index)
        })
        .ok_or_else(|| {
            AssetError::Other(format!(
                "system font family '{}' matched but its font data could not be read",
                query.family
            ))
        })?;
    parsed.map_err(|e| AssetError::Decode {
        src: format!("system:{}", query.family),
        msg: e.to_string(),
    })
}

/// wasm has no OS font database: the JS host must supply font bytes via
/// `bindSource` instead. A `system:` reference that reaches here (no
/// bytes were bound for it) fails with a clear runtime error.
#[cfg(target_arch = "wasm32")]
fn load_system_font(_query: &SystemFontQuery) -> Result<ezu_core::text::Font, AssetError> {
    Err(AssetError::Other(
        "system: fonts are not available on wasm; supply font bytes instead".to_string(),
    ))
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
/// `<source>` for per-tile scalar fields (DEM). A **neighbour** tile's
/// copy of a per-tile binding is keyed by suffixing that plain name with
/// `@<dx>,<dy>` (`dx`/`dy` ∈ `-1..=1`); see [`ezu_graph::neighbor`].
/// Cross-tile nodes (label collision) list the eight neighbour names in
/// `asset_inputs`, and a host that can fetch neighbour tiles binds them
/// via [`TileLoader::bind_mvt_neighbor`] / [`TileLoader::bind_features`].
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
        let opaque: OpaqueValue =
            Arc::new(crate::render::SharedLayer::new(layer)) as Arc<dyn Any + Send + Sync>;
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

    /// Bind a **neighbour** tile's MVT layers for cross-tile placement.
    /// Each layer binds under `<source>.<layer>@<dx>,<dy>` (see
    /// [`ezu_graph::neighbor`]); the layer geometry stays in the
    /// neighbour's own `[0, extent]` frame — the consuming node offsets
    /// it into the current tile's frame by `(dx, dy) × extent`. `(0, 0)`
    /// is rejected (use [`bind_mvt`](Self::bind_mvt) for the tile's own
    /// data).
    pub fn bind_mvt_neighbor(
        &mut self,
        source: &str,
        dx: i32,
        dy: i32,
        tile: DecodedTile,
    ) -> &mut Self {
        if dx == 0 && dy == 0 {
            return self.bind_mvt(source, tile);
        }
        for layer in tile.layers {
            let base = format!("{source}.{}", layer.name);
            self.bind_features(ezu_graph::neighbor_binding(&base, dx, dy), layer);
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

/// The distinct neighbour offsets `(dx, dy)` the graph requests for a
/// feature `source` — i.e. every `@<dx>,<dy>` suffix on a requested
/// binding whose base is `<source>.<layer>` (any layer). A host uses
/// this to fetch *only* the neighbour tiles the document's graph needs,
/// rather than the whole 3×3 window unconditionally. Offsets are
/// deduplicated and returned in a fixed (sorted) order.
pub fn requested_neighbor_offsets(
    requested: &std::collections::BTreeSet<String>,
    source: &str,
) -> Vec<(i32, i32)> {
    let prefix = format!("{source}.");
    let mut offs: Vec<(i32, i32)> = requested
        .iter()
        .filter_map(|name| {
            let (base, dx, dy) = ezu_graph::parse_neighbor_binding(name);
            ((dx, dy) != (0, 0) && base.starts_with(&prefix)).then_some((dx, dy))
        })
        .collect();
    offs.sort_unstable();
    offs.dedup();
    offs
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
                // Only HTTP brushes need pre-fetching — `file:` reads at
                // eval time, `builtin:` is registered by the host.
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
                // `system:` resolves by family name from the OS font
                // database (which reports its own face index), not by
                // fetching bytes — handle it here and skip the URL read.
                if let Some(spec) = font.url.strip_prefix("system:") {
                    let query =
                        parse_system_font(spec).map_err(|e| format!("font `{name}`: {e}"))?;
                    let face =
                        load_system_font(&query).map_err(|e| format!("font `{name}`: {e}"))?;
                    loader.fonts.insert(font.url.clone(), Arc::new(face));
                    continue;
                }
                let bytes = read_asset_bytes(&font.url, base_dir)
                    .await
                    .map_err(|e| format!("font `{name}`: {e}"))?;
                let face = ezu_core::text::Font::from_bytes(Arc::from(bytes), font.index)
                    .map_err(|e| format!("font `{name}`: {e}"))?;
                loader.fonts.insert(font.url.clone(), Arc::new(face));
            }
            ezu_style::SourceDecl::Glyphs(glyphs) => {
                // Nothing to fetch up front — ranges are text-driven and
                // pull lazily at eval time. Register the stack now so
                // relative `file:` templates resolve against `base_dir`
                // (the lazy path only handles absolute paths).
                let key = glyphs.asset_key();
                if loader
                    .glyphs
                    .read()
                    .expect("glyphs bank poisoned")
                    .contains_key(&key)
                {
                    continue;
                }
                let stack = match make_range_fetcher(&key, Some(base_dir.to_path_buf())) {
                    Some(fetcher) => ezu_core::text::SdfFontStack::with_fetcher(fetcher),
                    None => {
                        return Err(format!(
                            "glyphs `{name}`: unsupported url template `{}` — use \
                             `http(s)://…{{range}}.pbf` or `file:…{{range}}.pbf`",
                            glyphs.url
                        ))
                    }
                };
                loader.insert_glyphs(key, Arc::new(stack));
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
    use ezu_features::mvt::DecodedTile;
    use ezu_features::{Feature, FeatureLayer, Geometry};
    use std::collections::BTreeSet;

    fn point_layer(name: &str, pts: &[(i32, i32)]) -> FeatureLayer {
        FeatureLayer {
            name: name.into(),
            extent: 4096,
            features: pts
                .iter()
                .map(|&(x, y)| Feature {
                    id: None,
                    geometry: Geometry {
                        points: vec![(x, y)],
                        ..Default::default()
                    },
                    properties: Default::default(),
                })
                .collect(),
        }
    }

    #[test]
    fn neighbor_mvt_binds_under_offset_names() {
        let base = BrushBankLoader::new();
        let mut loader = TileLoader::new(&base, TileId { z: 3, x: 4, y: 5 });
        loader.bind_mvt(
            "roads",
            DecodedTile {
                layers: vec![point_layer("road", &[(10, 20)])],
            },
        );
        loader.bind_mvt_neighbor(
            "roads",
            1,
            0,
            DecodedTile {
                layers: vec![point_layer("road", &[(30, 40)])],
            },
        );

        // Own layer under the plain name, neighbour under `@1,0`.
        assert!(matches!(loader.load("roads.road"), Ok(Asset::Features(_))));
        assert!(matches!(
            loader.load("roads.road@1,0"),
            Ok(Asset::Features(_))
        ));
        // An unbound neighbour is a clean NotFound (→ centre-only, no error).
        assert!(matches!(
            loader.load("roads.road@-1,0"),
            Err(AssetError::NotFound(_))
        ));
        // Neighbour geometry stays in its own frame (offset applied by the
        // consumer, not at bind time).
        let Ok(Asset::Features(opq)) = loader.load("roads.road@1,0") else {
            panic!("neighbour bound");
        };
        let shared = opq.downcast::<crate::render::SharedLayer>().unwrap();
        assert_eq!(shared.layer.features[0].geometry.points, vec![(30, 40)]);
    }

    #[test]
    fn requested_offsets_filter_by_source() {
        let requested: BTreeSet<String> = [
            "roads.road",        // own
            "roads.road@1,0",    // east
            "roads.road@0,-1",   // north
            "roads.label@1,0",   // another layer, same offset
            "water.sea@1,1",     // different source
            "https://h/{range}", // an asset src, ignored
        ]
        .into_iter()
        .map(String::from)
        .collect();

        assert_eq!(
            requested_neighbor_offsets(&requested, "roads"),
            vec![(0, -1), (1, 0)]
        );
        assert_eq!(
            requested_neighbor_offsets(&requested, "water"),
            vec![(1, 1)]
        );
        assert_eq!(
            requested_neighbor_offsets(&requested, "absent"),
            Vec::<(i32, i32)>::new()
        );
    }

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
        let loader = BrushBankLoader::new();
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
        let loader = BrushBankLoader::new();
        match loader.load(&src).expect("font loads") {
            Asset::Font(opq) => {
                let font = opq
                    .downcast::<ezu_core::text::Font>()
                    .expect("payload is ezu_core::text::Font");
                let face = font.face();
                assert!(font.covers(&face, 'A'));
                assert!(!font.covers(&face, '0')); // digits live in the other subset
            }
            other => panic!("expected a Font asset, got {other:?}"),
        }
    }

    #[test]
    fn glyphs_template_loads_lazily_and_hashes_its_ranges() {
        // A `file:` glyphs URL template over the vendored ezu-core test
        // range (see ../ezu-core/tests/glyphs/README.md).
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ezu-core/tests/glyphs");
        let src = format!("file:{}/{{range}}.pbf", dir.display()).replace('\\', "/");
        let loader = BrushBankLoader::new();

        let Asset::Glyphs(opq) = loader.load(&src).expect("template loads") else {
            panic!("expected a Glyphs asset from a {{range}} template");
        };
        let stack = opq
            .downcast::<ezu_core::text::SdfFontStack>()
            .expect("payload is an SdfFontStack");

        // Ranges pull lazily from disk on first use, and the loader's
        // hash tracks the grown range set (the eval-cache key input).
        let before = loader.hash(&src);
        assert!(!stack.is_loaded(0));
        assert!(stack.glyph('A').is_some(), "0-255.pbf fetches on demand");
        assert!(stack.is_loaded(0));
        assert_ne!(loader.hash(&src), before, "hash must follow the ranges");

        // The same key resolves to the same stack (ranges are shared).
        let Asset::Glyphs(again) = loader.load(&src).expect("reload") else {
            panic!("expected a Glyphs asset");
        };
        let again = again
            .downcast::<ezu_core::text::SdfFontStack>()
            .expect("payload is an SdfFontStack");
        assert!(Arc::ptr_eq(&stack, &again));
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
    fn system_font_parses_family_weight_and_style() {
        // Bare family, defaults applied.
        let q = parse_system_font("Arial Unicode MS").unwrap();
        assert_eq!(q.family, "Arial Unicode MS");
        assert_eq!(q.weight, 400);
        assert_eq!(q.style, SystemFontStyle::Normal);

        // Percent-encoded spaces decode to the same family.
        assert_eq!(
            parse_system_font("Arial%20Unicode%20MS").unwrap().family,
            "Arial Unicode MS"
        );

        // Weight + style, style is case-insensitive.
        let q = parse_system_font("Helvetica?weight=700&style=Italic").unwrap();
        assert_eq!(q.family, "Helvetica");
        assert_eq!(q.weight, 700);
        assert_eq!(q.style, SystemFontStyle::Italic);

        assert_eq!(
            parse_system_font("Noto%20Sans?style=oblique")
                .unwrap()
                .style,
            SystemFontStyle::Oblique
        );
    }

    #[test]
    fn system_font_rejects_bad_input() {
        // Empty family.
        assert!(parse_system_font("").is_err());
        assert!(parse_system_font("?weight=400").is_err());

        // Non-numeric / out-of-range weight.
        assert!(parse_system_font("Helvetica?weight=bold").is_err());
        assert!(parse_system_font("Helvetica?weight=50").is_err());
        assert!(parse_system_font("Helvetica?weight=1000").is_err());

        // Unknown style value and unknown query key.
        assert!(parse_system_font("Helvetica?style=slanted").is_err());
        assert!(parse_system_font("Helvetica?size=12").is_err());

        // Malformed query part (no `=`).
        assert!(parse_system_font("Helvetica?weight").is_err());
    }

    /// Resolution goes through a `fontdb::Database` seeded with a fixture
    /// font (not the host's system fonts), so the query→bytes→face path
    /// is exercised deterministically on every platform.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn system_font_resolves_from_a_seeded_fontdb() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
        let bytes = std::fs::read(&path).expect("fixture font readable");
        let mut db = fontdb::Database::new();
        db.load_font_data(bytes);

        // The fixture's name-table family (as `fontdb` sees it).
        let family = db
            .faces()
            .next()
            .and_then(|f| f.families.first().map(|(name, _)| name.clone()))
            .expect("one seeded face");

        let query = parse_system_font(&family).unwrap();
        let font = resolve_system_font(&db, &query).expect("fixture family resolves");
        let face = font.face();
        assert!(font.covers(&face, 'A'));

        // A family that isn't installed is a clean, described miss.
        let missing = parse_system_font("No Such Family 12345").unwrap();
        match resolve_system_font(&db, &missing) {
            Err(AssetError::Other(msg)) => assert!(msg.contains("not found"), "{msg}"),
            other => panic!("expected a not-found error, got {other:?}"),
        }
    }

    /// A real system font, off by default (installed set is host-specific).
    /// Run with `cargo test -- --ignored` on a machine that has the family.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    #[ignore = "depends on host-installed fonts"]
    fn system_font_resolves_from_installed_fonts() {
        let query = parse_system_font("Arial Unicode MS").unwrap();
        let font = load_system_font(&query).expect("Arial Unicode MS is installed");
        let face = font.face();
        assert!(font.covers(&face, 'A'));
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
