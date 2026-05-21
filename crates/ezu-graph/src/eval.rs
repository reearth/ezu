//! Evaluation context, asset loader, and error types used during
//! `Node::eval`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::buf::{OpaqueValue, RasterBuf};
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
#[derive(Debug, Clone)]
pub enum Asset {
    Image(Arc<RasterBuf>),
    Brush(OpaqueValue),
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

/// Pluggable backend for resolving `src:` strings to bytes/decoded
/// assets. Implementations live in the host (CLI / server / WASM).
pub trait AssetLoader: Send + Sync {
    fn load(&self, src: &str) -> Result<Asset, AssetError>;
}

/// A no-op asset loader. Every load returns `NotFound`. Useful for
/// tests of graphs that don't touch any asset.
pub struct NoAssets;
impl AssetLoader for NoAssets {
    fn load(&self, src: &str) -> Result<Asset, AssetError> {
        Err(AssetError::NotFound(src.to_string()))
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
    /// Host-supplied tile data (e.g. a decoded MVT). Source nodes
    /// downcast this to the concrete type they expect. `None` means no
    /// tile data is available (e.g. the host fetched nothing for this
    /// tile); source nodes should produce an empty result.
    pub tile_data: Option<&'a crate::buf::OpaqueValue>,
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
