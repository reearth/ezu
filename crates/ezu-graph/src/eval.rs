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
/// `Raster` ports must produce; the final tile is the inner `tile_size`
/// region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CanvasInfo {
    pub tile_size: u32,
    pub pad: u32,
}

impl CanvasInfo {
    pub fn padded_size(&self) -> u32 {
        self.tile_size + 2 * self.pad
    }
}

/// One asset fetched by an [`AssetLoader`].
///
/// The shape is uniform across input kinds (images, brushes, feature
/// layers, …) so every source-style node consumes the host through the
/// same trait — like a shader sampling a typed uniform binding.
/// `Features` carries a type-erased payload; by convention the
/// concrete type is `Arc<ezu_features::FeatureLayer>`. `Font` is
/// likewise type-erased (by convention `Arc<ezu_core::text::Font>`) so
/// this crate gains no font or raster-drawing dependencies.
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
pub struct EvalCtx<'a> {
    pub tile: TileId,
    pub canvas: CanvasInfo,
    pub assets: &'a dyn AssetLoader,
    pub params: &'a ParamValues,
    /// Deterministic root seed for this render. World-anchored nodes
    /// hash this with world coordinates to produce per-feature seeds.
    pub rng_seed: u64,
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
