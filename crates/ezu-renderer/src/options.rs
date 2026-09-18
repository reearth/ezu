//! What a host passes in: per-binding and per-render options.
//!
//! Each shell reads these out of whatever its host speaks — a JS object,
//! a JSON string — and hands over the struct. Keeping the structs here
//! is what stops two shells from growing two different sets of options.

use ezu_paint::host::PngCompression;

/// Encoding of the bytes `render_tile` hands back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputFormat {
    /// PNG, compressed per [`RenderOptions::png_compression`].
    #[default]
    Png,
    /// Lossless WebP.
    Webp,
    /// Straight un-premultiplied 8-bit RGBA, no container.
    Rgba,
}

/// Per-render options: output encoding, canvas overrides, parameters.
#[derive(Clone, Debug)]
pub struct RenderOptions {
    /// Encoding of the returned bytes.
    pub format: OutputFormat,
    /// Override the style's canvas size for this render (hi-DPI,
    /// previews). `None` keeps the style's `tile-size`.
    pub tile_size: Option<u32>,
    /// Override the margin rendered outside the tile. `None` takes the
    /// style's `pad`, floored by what the graph's filters reach for.
    pub pad: Option<u32>,
    /// Effort the PNG encoder spends. Ignored by the other formats.
    pub png_compression: PngCompression,
    /// Opt into the parallel evaluator. Only meaningful when the crate
    /// is built with `parallel` *and* the host has a thread pool;
    /// otherwise evaluation falls back to sequential, which produces the
    /// same bytes.
    pub parallel: bool,
    /// Render-time overrides for the style's `params`, as
    /// `(name, value-as-text)` pairs. Kept as text so the same parser
    /// the CLI's `--param` uses can validate them against the
    /// declarations — same coercions, same error messages.
    pub params: Vec<(String, String)>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            format: OutputFormat::Png,
            tile_size: None,
            pad: None,
            png_compression: PngCompression::Default,
            parallel: false,
            params: Vec::new(),
        }
    }
}

/// Per-binding options: which tile of the neighbourhood these bytes are,
/// and where they came from.
#[derive(Clone, Debug, Default)]
pub struct BindOptions {
    /// The `(dx, dy)` offset within the 3×3 neighbourhood these bytes
    /// belong to. `(0, 0)`, the default, is the tile being rendered.
    pub coord: (i32, i32),
    /// The zoom the bytes are natively encoded at, when it is shallower
    /// than the tile being rendered — a source that stops at its
    /// `max-zoom` while the host serves deeper tiles. `None` means the
    /// bytes are already in the requested tile's frame.
    pub source_zoom: Option<u8>,
    /// A sprite source's index document, when the style gives a URL for
    /// it rather than inlining it. Ignored by every other source kind.
    pub index: Option<String>,
}
