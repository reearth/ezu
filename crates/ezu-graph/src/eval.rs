//! Evaluation context, asset loader, and error types used during
//! `Node::eval`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::buf::{OpaqueValue, RasterBuf, ScalarField, SpriteSheet};
use crate::value::ScalarValue;

/// Tile coordinate (z/x/y in TMS-ish form; the meaning is up to the host).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

/// Per-render canvas geometry. The padded buffer is the actual size all
/// `Raster` ports must produce; the final tile is the inner
/// `tile_w` × `tile_h` region.
///
/// A map tile is square and [`square`](Self::square) is how one is
/// asked for. The two axes are separate because not every render is a
/// tile: a legend swatch is whatever shape the legend has room for, and
/// its geometry is synthetic, so there is no projection to distort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CanvasInfo {
    pub tile_w: u32,
    pub tile_h: u32,
    pub pad: u32,
}

impl CanvasInfo {
    /// The usual case: a square tile of `tile_size` px with `pad` px of
    /// margin on every side.
    pub fn square(tile_size: u32, pad: u32) -> Self {
        Self {
            tile_w: tile_size,
            tile_h: tile_size,
            pad,
        }
    }

    pub fn padded_w(&self) -> u32 {
        self.tile_w + 2 * self.pad
    }

    pub fn padded_h(&self) -> u32 {
        self.tile_h + 2 * self.pad
    }

    /// Both padded axes at once — the shape every `Raster` port must
    /// produce.
    pub fn padded_dims(&self) -> (u32, u32) {
        (self.padded_w(), self.padded_h())
    }
}

/// One asset fetched by an [`AssetLoader`].
///
/// The shape is uniform across input kinds (images, brushes, feature
/// layers, …) so every source-style node consumes the host through the
/// same trait — like a shader sampling a typed uniform binding.
/// `Features` carries a type-erased payload; by convention the
/// concrete type is `Arc<ezu_features::FeatureLayer>`. `Font` and
/// `Glyphs` are likewise type-erased (by convention
/// `Arc<ezu_core::text::Font>` / `Arc<ezu_core::text::SdfFontStack>`)
/// so this crate gains no font or raster-drawing dependencies.
#[derive(Debug, Clone)]
pub enum Asset {
    Image(Arc<RasterBuf>),
    Brush(OpaqueValue),
    Features(OpaqueValue),
    ScalarField(Arc<ScalarField>),
    /// A sprite atlas + name→rect index; the `icon` node crops named rects.
    Sprite(Arc<SpriteSheet>),
    /// A loaded font face; the `text` node shapes and draws with it.
    Font(OpaqueValue),
    /// A glyph-PBF (SDF) fontstack; the `text` node's compat backend.
    Glyphs(OpaqueValue),
}

#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("asset not found: `{0}`")]
    NotFound(String),
    #[error("asset decode failed for `{src}`: {msg}")]
    Decode { src: String, msg: String },
    #[error("asset error: {0}")]
    Other(String),
}

/// Pluggable backend for resolving named asset bindings (images,
/// brushes, tile features, …). Names without a scheme prefix
/// (`<source>` or `<source>.<layer>`) are by convention tile-scoped —
/// the host rebinds them per render. Asset srcs carry a scheme
/// (`builtin:`, `file:`, `http(s)://`) and are document-scoped.
///
/// `hash` returns a stable content/identity hash the evaluator folds
/// into every consuming node's cache key, so changes in a bound asset
/// invalidate caches automatically. Implementations may return `0` if
/// the binding never changes (document-scoped, fixed disk file, etc.).
pub trait AssetLoader: Send + Sync {
    fn load(&self, name: &str) -> Result<Asset, AssetError>;

    /// Content/identity hash for cache invalidation. The default of
    /// `0` is safe for assets that never change for the lifetime of a
    /// loader (typical for in-memory image / brush banks).
    fn hash(&self, _name: &str) -> u128 {
        0
    }
}

/// A no-op asset loader. Every load returns `NotFound`. Useful for
/// tests of graphs that don't touch any asset.
pub struct NoAssets;
impl AssetLoader for NoAssets {
    fn load(&self, name: &str) -> Result<Asset, AssetError> {
        Err(AssetError::NotFound(name.to_string()))
    }
}

/// Resolved parameter values for one render. Nodes look up `$name`
/// references here.
#[derive(Debug, Default, Clone)]
pub struct ParamValues {
    pub values: HashMap<String, ScalarValue>,
}

impl ParamValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, name: impl Into<String>, value: ScalarValue) {
        self.values.insert(name.into(), value);
    }

    pub fn get(&self, name: &str) -> Option<ScalarValue> {
        self.values.get(name).copied()
    }
}

/// Read-only environment a node sees during `eval`.
#[derive(Clone, Copy)]
pub struct EvalCtx<'a> {
    pub tile: TileId,
    pub canvas: CanvasInfo,
    pub assets: &'a dyn AssetLoader,
    pub params: &'a ParamValues,
    /// Deterministic root seed for this render. World-anchored nodes
    /// hash this with world coordinates to produce per-feature seeds.
    pub rng_seed: u64,
    /// How far outside the canvas *this* node's geometry can still reach
    /// the rendered tile — see [`crate::Node::influence_pad`]. A source
    /// may drop geometry that lies further out than this; `u32::MAX`
    /// means the reach is unbounded and nothing may be dropped.
    pub influence_pad: u32,
}

impl EvalCtx<'_> {
    /// The canvas rectangle, grown by this node's influence, outside
    /// which geometry cannot affect the rendered tile. In padded-canvas
    /// pixels; `None` when the reach is unbounded.
    pub fn cull_rect(&self) -> Option<(f64, f64, f64, f64)> {
        if self.influence_pad == u32::MAX {
            return None;
        }
        let m = self.influence_pad as f64;
        Some((
            -m,
            -m,
            self.canvas.padded_w() as f64 + m,
            self.canvas.padded_h() as f64 + m,
        ))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("input port `{0}` was not supplied")]
    MissingInput(String),
    #[error("input `{port}` has wrong kind: expected {expected:?}, got {got:?}")]
    InputKindMismatch {
        port: String,
        expected: crate::port::PortKind,
        got: crate::port::PortKind,
    },
    #[error(transparent)]
    Asset(#[from] AssetError),
    #[error("{0}")]
    Other(String),
}
