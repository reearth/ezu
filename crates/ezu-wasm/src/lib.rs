//! WebAssembly bindings for the ezu painterly map renderer.
//!
//! The JS side owns all I/O (HTTP, PMTiles, asset fetching). This crate
//! exposes a stateful [`Renderer`] that:
//!
//! 1. holds a parsed Ezu Style document,
//! 2. holds a brush bank populated by repeated calls to `register_brush`,
//! 3. renders a single tile from raw MVT bytes via [`Renderer::render`],
//!    returning the encoded PNG bytes.
//!
//! All `Result`s flow back through [`JsError`] so JS gets idiomatic
//! `try { … } catch (e) { … }` semantics.

use std::collections::HashMap;

use ezu_core::TileId;
use ezu_paint::{canvas_from_style, encode_png, render_style, Brush};
use ezu_style::Style;
use wasm_bindgen::prelude::*;

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
    pub fn new(style_json: &str) -> Result<Renderer, JsError> {
        #[cfg(feature = "panic-hook")]
        console_error_panic_hook::set_once();

        let style = Style::from_json(style_json).map_err(jserr)?;
        Ok(Self {
            style,
            brushes: HashMap::new(),
        })
    }

    /// Replace the active style. Returns the new layer count for convenience.
    #[wasm_bindgen(js_name = setStyle)]
    pub fn set_style(&mut self, style_json: &str) -> Result<usize, JsError> {
        let style = Style::from_json(style_json).map_err(jserr)?;
        self.style = style;
        Ok(self.style.layers.len())
    }

    /// Register a `.myb` brush under `name` so layers can refer to it as
    /// `"brush": "@name"` (or just `"name"`).
    #[wasm_bindgen(js_name = registerBrush)]
    pub fn register_brush(&mut self, name: &str, myb_json: &str) -> Result<(), JsError> {
        let brush = hokusai::myb::from_str(myb_json).map_err(jserr)?;
        self.brushes.insert(name.to_string(), brush);
        Ok(())
    }

    /// Number of brushes currently registered.
    #[wasm_bindgen(js_name = brushCount)]
    pub fn brush_count(&self) -> usize {
        self.brushes.len()
    }

    /// Render a single tile.
    ///
    /// - `mvt_bytes` — decompressed MVT bytes (JS handles gzip / range fetch).
    /// - `(z, x, y)` — standard slippy tile coordinate.
    ///
    /// Returns PNG-encoded bytes sized to `style.tile-size`.
    pub fn render(&self, mvt_bytes: &[u8], z: u8, x: u32, y: u32) -> Result<Vec<u8>, JsError> {
        let tile = TileId::new(z, x, y);
        let decoded = ezu_mvt::decode(mvt_bytes).map_err(jserr)?;
        let mut canvas = canvas_from_style(&self.style);
        let resolver = |name: &str| -> Option<&Brush> {
            let key = name.strip_prefix('@').unwrap_or(name);
            self.brushes.get(key)
        };
        render_style(&mut canvas, &self.style, &decoded, tile, &resolver).map_err(jserr)?;
        Ok(encode_png(&canvas).map_err(jserr)?)
    }

    /// Render a tile that has no MVT data (out-of-range, miss, etc.).
    /// Useful for filling the viewport with the style's paper background.
    #[wasm_bindgen(js_name = renderBlank)]
    pub fn render_blank(&self) -> Result<Vec<u8>, JsError> {
        let canvas = canvas_from_style(&self.style);
        Ok(encode_png(&canvas).map_err(jserr)?)
    }

    /// `tile-size` declared by the current style.
    #[wasm_bindgen(getter, js_name = tileSize)]
    pub fn tile_size(&self) -> u32 {
        self.style.tile_size
    }
}

/// Whether the wasm binary was compiled with `+simd128`. Lets the demo
/// page label the renderer build accurately.
#[wasm_bindgen(js_name = simdEnabled)]
pub fn simd_enabled() -> bool {
    cfg!(target_feature = "simd128")
}

fn jserr<E: std::fmt::Display>(e: E) -> JsError {
    JsError::new(&e.to_string())
}
