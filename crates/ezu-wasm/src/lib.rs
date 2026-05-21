//! WebAssembly bindings for the ezu painterly map renderer.
//!
//! The JS side owns all I/O (HTTP, PMTiles, asset fetching). This crate
//! exposes a stateful [`Renderer`] that holds a parsed Ezu Style document
//! plus a brush bank, and renders one tile at a time.
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
//! All fallible methods throw a JavaScript `Error` whose `.name` discriminates
//! the failure kind: `InvalidStyle`, `BrushParse`, `MvtDecode`, `RenderFailed`,
//! `PngEncode`. JS code can `try { … } catch (e) { if (e.name === "InvalidStyle") … }`.

use std::collections::HashMap;

use ezu_core::TileId;
use ezu_paint::{
    canvas_from_style, canvas_from_style_sized, encode_png, render_style, to_rgba8, Brush,
};
use ezu_style::Style;
use wasm_bindgen::prelude::*;

const ERR_STYLE: &str = "InvalidStyle";
const ERR_BRUSH: &str = "BrushParse";
const ERR_MVT: &str = "MvtDecode";
const ERR_RENDER: &str = "RenderFailed";
const ERR_PNG: &str = "PngEncode";

/// Stateful WASM renderer.
#[wasm_bindgen]
pub struct Renderer {
    style: Style,
    brushes: HashMap<String, Brush>,
}

#[wasm_bindgen]
impl Renderer {
    /// Build a renderer from an Ezu Style JSON document.
    #[wasm_bindgen(constructor)]
    pub fn new(style_json: &str) -> Result<Renderer, JsValue> {
        #[cfg(feature = "panic-hook")]
        console_error_panic_hook::set_once();

        let style = Style::from_json(style_json).map_err(|e| named_err(ERR_STYLE, e))?;
        Ok(Self {
            style,
            brushes: HashMap::new(),
        })
    }

    /// Replace the active style. Returns the new layer count.
    #[wasm_bindgen(js_name = setStyle)]
    pub fn set_style(&mut self, style_json: &str) -> Result<usize, JsValue> {
        let style = Style::from_json(style_json).map_err(|e| named_err(ERR_STYLE, e))?;
        self.style = style;
        Ok(self.style.layers.len())
    }

    /// Register a `.myb` brush under `name` so layers can refer to it as
    /// `"brush": "@name"` (the `@` prefix is stripped at resolve time).
    /// Re-registering the same name replaces the previous entry.
    #[wasm_bindgen(js_name = registerBrush)]
    pub fn register_brush(&mut self, name: &str, myb_json: &str) -> Result<(), JsValue> {
        let brush = hokusai::myb::from_str(myb_json).map_err(|e| named_err(ERR_BRUSH, e))?;
        self.brushes.insert(name.to_string(), brush);
        Ok(())
    }

    /// Remove a brush by name. Returns `true` if the brush existed.
    #[wasm_bindgen(js_name = unregisterBrush)]
    pub fn unregister_brush(&mut self, name: &str) -> bool {
        self.brushes.remove(name).is_some()
    }

    /// Drop every registered brush.
    #[wasm_bindgen(js_name = clearBrushes)]
    pub fn clear_brushes(&mut self) {
        self.brushes.clear();
    }

    /// Number of brushes currently registered.
    #[wasm_bindgen(js_name = brushCount)]
    pub fn brush_count(&self) -> usize {
        self.brushes.len()
    }

    /// `tile-size` declared by the current style.
    #[wasm_bindgen(getter, js_name = tileSize)]
    pub fn tile_size(&self) -> u32 {
        self.style.tile_size
    }

    /// Render a single tile to PNG bytes using the style's `tile-size` / `pad`.
    /// Pass `null` for `mvtBytes` to get a paper-only tile.
    pub fn render(
        &self,
        mvt_bytes: Option<Vec<u8>>,
        z: u8,
        x: u32,
        y: u32,
    ) -> Result<Vec<u8>, JsValue> {
        self.render_inner(mvt_bytes.as_deref(), z, x, y, None, OutputFormat::Png)
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
        self.render_inner(mvt_bytes.as_deref(), z, x, y, None, OutputFormat::Rgba)
    }

    /// Like `render` but with `tile_size` / `pad` overridden — useful for
    /// hi-DPI / preview rendering without mutating the style.
    #[wasm_bindgen(js_name = renderAt)]
    pub fn render_at(
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
            OutputFormat::Png,
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
        )
    }
}

#[derive(Clone, Copy)]
enum OutputFormat {
    Png,
    Rgba,
}

impl Renderer {
    fn render_inner(
        &self,
        mvt_bytes: Option<&[u8]>,
        z: u8,
        x: u32,
        y: u32,
        size_override: Option<(u32, u32)>,
        format: OutputFormat,
    ) -> Result<Vec<u8>, JsValue> {
        let mut canvas = match size_override {
            Some((ts, pad)) => canvas_from_style_sized(&self.style, ts, pad),
            None => canvas_from_style(&self.style),
        };

        if let Some(bytes) = mvt_bytes {
            let tile = TileId::new(z, x, y);
            let decoded = ezu_mvt::decode(bytes).map_err(|e| named_err(ERR_MVT, e))?;
            let resolver = |name: &str| -> Option<&Brush> {
                let key = name.strip_prefix('@').unwrap_or(name);
                self.brushes.get(key)
            };
            render_style(&mut canvas, &self.style, &decoded, tile, &resolver)
                .map_err(|e| named_err(ERR_RENDER, e))?;
        }

        Ok(match format {
            OutputFormat::Png => encode_png(&canvas).map_err(|e| named_err(ERR_PNG, e))?,
            OutputFormat::Rgba => to_rgba8(&canvas),
        })
    }
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
