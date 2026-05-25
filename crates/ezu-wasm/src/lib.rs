//! WebAssembly bindings for the ezu painterly map renderer (Style).
//!
//! The JS side owns all I/O (HTTP, PMTiles, asset fetching). This crate
//! exposes a stateful [`Renderer`] that holds a parsed style document,
//! its built graph, and an in-memory brush bank — it renders one tile
//! at a time given the raw MVT bytes for that tile.
//!
//! ## Output formats
//!
//! - `render` / `renderAt` return PNG bytes — useful for `<img>` or
//!   `URL.createObjectURL`.
//! - `renderRgba` / `renderRgbaAt` return straight (un-premultiplied)
//!   8-bit RGBA bytes (`tile_w * tile_h * 4`) — feed directly to
//!   `new ImageData(new Uint8ClampedArray(buf.buffer), w, h)` and then
//!   `ctx.putImageData(...)` to skip the PNG decode round trip.
//!
//! ## Missing tiles
//!
//! Pass `null` / `undefined` as `mvtBytes` to render the style's paper
//! background only (out-of-range tiles, archive misses, etc.).
//!
//! ## Errors
//!
//! All fallible methods throw a JavaScript `Error` whose `.name`
//! discriminates the failure kind: `InvalidStyle`, `BrushParse`,
//! `MvtDecode`, `RenderFailed`, `PngEncode`.

mod log;

pub use log::LogSink;

use std::sync::Arc;

use ezu_graph::{build_graph, Cache, CanvasInfo, Evaluator, Graph, ParamValues, PortValue, TileId};
use ezu_paint::host::{
    raster_to_png_with, raster_to_rgba8, raster_to_webp, BrushBankLoader, PngCompression,
    TileLoader,
};
use ezu_paint::nodes::default_registry;
use ezu_style::Document;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

const ERR_STYLE: &str = "InvalidStyle";
const ERR_BRUSH: &str = "BrushParse";
const ERR_MVT: &str = "MvtDecode";
const ERR_RENDER: &str = "RenderFailed";
const ERR_PNG: &str = "PngEncode";
const ERR_WEBP: &str = "WebpEncode";

/// Stateful WASM renderer.
#[wasm_bindgen]
pub struct Renderer {
    doc: Document,
    graph: Arc<Graph>,
    cache: Arc<Cache>,
    assets: BrushBankLoader,
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
        })
    }

    /// Replace the active style. Returns the new node count. Invalidates
    /// the intermediate cache.
    #[wasm_bindgen(js_name = setStyle)]
    pub fn set_style(&mut self, style_json: &str) -> Result<usize, JsValue> {
        let (doc, graph) = parse_and_build(style_json)?;
        let n = doc.nodes.len();
        self.doc = doc;
        self.graph = Arc::new(graph);
        self.cache = Arc::new(Cache::new());
        Ok(n)
    }

    /// Register a `.myb` brush under `name` so a `brush-file` node with
    /// `src: "@name"` (via the style's `assets` map) resolves to it.
    /// Re-registering the same name replaces the previous entry.
    #[wasm_bindgen(js_name = registerBrush)]
    pub fn register_brush(&mut self, name: &str, myb_json: &str) -> Result<(), JsValue> {
        let brush = hokusai::myb::from_str(myb_json).map_err(|e| named_err(ERR_BRUSH, e))?;
        self.assets.insert(name.to_string(), brush);
        Ok(())
    }

    /// Remove a brush by name. Returns `true` if the brush existed.
    #[wasm_bindgen(js_name = unregisterBrush)]
    pub fn unregister_brush(&mut self, name: &str) -> bool {
        self.assets.bank.remove(name).is_some()
    }

    /// Drop every registered brush.
    #[wasm_bindgen(js_name = clearBrushes)]
    pub fn clear_brushes(&mut self) {
        self.assets.bank.clear();
    }

    /// Number of brushes currently registered.
    #[wasm_bindgen(js_name = brushCount)]
    pub fn brush_count(&self) -> usize {
        self.assets.bank.len()
    }

    /// `tile-size` declared by the current style.
    #[wasm_bindgen(getter, js_name = tileSize)]
    pub fn tile_size(&self) -> u32 {
        self.doc.tile_size
    }

    /// Render a single tile to PNG bytes using the style's `tile-size` / `pad`.
    /// Pass `null` for `mvtBytes` to get a paper-only tile.
    ///
    /// Optional `options`: `{ png: { compression: 'fast' | 'default' | 'best' } }`.
    pub fn render(
        &self,
        mvt_bytes: Option<Vec<u8>>,
        z: u8,
        x: u32,
        y: u32,
        options: Option<js_sys::Object>,
    ) -> Result<Vec<u8>, JsValue> {
        self.render_inner(
            mvt_bytes.as_deref(),
            z,
            x,
            y,
            None,
            OutputFormat::Png,
            parse_options(options.as_ref()),
        )
    }

    /// Render a single tile to lossless WebP bytes. Smaller than PNG
    /// for the same painterly content and decoded natively by every
    /// modern browser. WebP is lossless-only via the pure-Rust
    /// `image-webp` codec; for lossy WebP, see the
    /// `OffscreenCanvas.convertToBlob` recipe in the crate README.
    #[allow(unused_variables)]
    #[wasm_bindgen(js_name = renderWebp)]
    pub fn render_webp(
        &self,
        mvt_bytes: Option<Vec<u8>>,
        z: u8,
        x: u32,
        y: u32,
        options: Option<js_sys::Object>,
    ) -> Result<Vec<u8>, JsValue> {
        self.render_inner(
            mvt_bytes.as_deref(),
            z,
            x,
            y,
            None,
            OutputFormat::Webp,
            EncodeOptions::default(),
        )
    }

    /// Render a single tile to straight RGBA8 bytes (`tile_w * tile_h * 4`).
    #[wasm_bindgen(js_name = renderRgba)]
    pub fn render_rgba(
        &self,
        mvt_bytes: Option<Vec<u8>>,
        z: u8,
        x: u32,
        y: u32,
    ) -> Result<Vec<u8>, JsValue> {
        self.render_inner(
            mvt_bytes.as_deref(),
            z,
            x,
            y,
            None,
            OutputFormat::Rgba,
            EncodeOptions::default(),
        )
    }

    /// Like `render` but with `tile_size` / `pad` overridden — useful for
    /// hi-DPI / preview rendering without mutating the style.
    #[wasm_bindgen(js_name = renderAt)]
    #[allow(clippy::too_many_arguments)]
    pub fn render_at(
        &self,
        mvt_bytes: Option<Vec<u8>>,
        z: u8,
        x: u32,
        y: u32,
        tile_size: u32,
        pad: u32,
        options: Option<js_sys::Object>,
    ) -> Result<Vec<u8>, JsValue> {
        self.render_inner(
            mvt_bytes.as_deref(),
            z,
            x,
            y,
            Some((tile_size, pad)),
            OutputFormat::Png,
            parse_options(options.as_ref()),
        )
    }

    /// Like `renderWebp` but with `tile_size` / `pad` overridden.
    #[allow(unused_variables)]
    #[wasm_bindgen(js_name = renderWebpAt)]
    #[allow(clippy::too_many_arguments)]
    pub fn render_webp_at(
        &self,
        mvt_bytes: Option<Vec<u8>>,
        z: u8,
        x: u32,
        y: u32,
        tile_size: u32,
        pad: u32,
        options: Option<js_sys::Object>,
    ) -> Result<Vec<u8>, JsValue> {
        self.render_inner(
            mvt_bytes.as_deref(),
            z,
            x,
            y,
            Some((tile_size, pad)),
            OutputFormat::Webp,
            EncodeOptions::default(),
        )
    }

    /// Like `renderRgba` but with `tile_size` / `pad` overridden.
    #[wasm_bindgen(js_name = renderRgbaAt)]
    pub fn render_rgba_at(
        &self,
        mvt_bytes: Option<Vec<u8>>,
        z: u8,
        x: u32,
        y: u32,
        tile_size: u32,
        pad: u32,
    ) -> Result<Vec<u8>, JsValue> {
        self.render_inner(
            mvt_bytes.as_deref(),
            z,
            x,
            y,
            Some((tile_size, pad)),
            OutputFormat::Rgba,
            EncodeOptions::default(),
        )
    }
}

#[derive(Clone, Copy)]
enum OutputFormat {
    Png,
    Webp,
    Rgba,
}

#[derive(Clone, Copy, Default)]
struct EncodeOptions {
    png_compression: PngCompression,
}

/// Pull encoding options out of the JS `{ png: { compression } }`
/// object. Unknown keys are silently ignored so adding fields later
/// stays backwards-compatible; an unrecognised `compression` value
/// falls back to the default rather than throwing — JS callers
/// typically discover the right strings empirically.
fn parse_options(obj: Option<&js_sys::Object>) -> EncodeOptions {
    let mut out = EncodeOptions::default();
    let Some(obj) = obj else {
        return out;
    };
    let png = js_sys::Reflect::get(obj, &"png".into()).unwrap_or(JsValue::UNDEFINED);
    if let Some(png_obj) = png.dyn_ref::<js_sys::Object>() {
        let comp =
            js_sys::Reflect::get(png_obj, &"compression".into()).unwrap_or(JsValue::UNDEFINED);
        if let Some(s) = comp.as_string() {
            out.png_compression = match s.as_str() {
                "fast" => PngCompression::Fast,
                "best" => PngCompression::Best,
                _ => PngCompression::Default,
            };
        }
    }
    out
}

impl Renderer {
    #[allow(clippy::too_many_arguments)]
    fn render_inner(
        &self,
        mvt_bytes: Option<&[u8]>,
        z: u8,
        x: u32,
        y: u32,
        size_override: Option<(u32, u32)>,
        format: OutputFormat,
        options: EncodeOptions,
    ) -> Result<Vec<u8>, JsValue> {
        let (tile_size, pad) = size_override.unwrap_or((self.doc.tile_size, self.doc.pad));
        let tile_id = TileId { z, x, y };
        let mut tile_loader = TileLoader::new(&self.assets, tile_id);
        if let Some(bytes) = mvt_bytes {
            tile_loader
                .bind_mvt(ezu_features::mvt::decode(bytes).map_err(|e| named_err(ERR_MVT, e))?);
        }

        let ev = Evaluator::new(&self.graph, &self.cache, &tile_loader);
        let out = ev
            .render(
                tile_id,
                CanvasInfo { tile_size, pad },
                &ParamValues::new(),
                tile_seed(z, x, y),
            )
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

        Ok(match format {
            OutputFormat::Png => {
                raster_to_png_with(&raster, tile_size, pad, options.png_compression)
                    .map_err(|e| named_err(ERR_PNG, e))?
            }
            OutputFormat::Webp => {
                raster_to_webp(&raster, tile_size, pad).map_err(|e| named_err(ERR_WEBP, e))?
            }
            OutputFormat::Rgba => raster_to_rgba8(&raster, tile_size, pad),
        })
    }
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

/// Build a JS `Error` whose `.name` discriminates the failure kind so callers
/// can dispatch on it.
fn named_err(name: &str, e: impl std::fmt::Display) -> JsValue {
    let err = js_sys::Error::new(&e.to_string());
    err.set_name(name);
    err.into()
}
