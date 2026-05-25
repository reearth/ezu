//! Typed DAG evaluator for the Ezu Style Spec.
//!
//! - [`port`] — `PortKind`, `PortSpec`, `CoordSpace`
//! - [`buf`] / [`value`] — concrete buffers and the `PortValue` enum
//! - [`node`] — the `Node` trait every operation implements
//! - [`graph`] — `GraphBuilder` / `Graph` with topo sort, type checking,
//!   cycle detection, and pad propagation
//! - [`registry`] / [`build`] — turn a parsed `ezu_style::Document`
//!   into a typed `Graph` using a registry of node factories
//! - [`eval`] / [`cache`] / [`evaluator`] — render a tile

pub mod buf;
pub mod build;
pub mod cache;
pub mod eval;
pub mod evaluator;
pub mod graph;
pub mod node;
pub mod port;
pub mod registry;
pub mod value;

pub use buf::{HeightField, OpaqueValue, RasterBuf};
pub use build::{build_graph, BuildGraphError};
pub use cache::{Cache, CacheKey, Hash128};
pub use eval::{
    Asset, AssetError, AssetLoader, CanvasInfo, EvalCtx, EvalError, NoAssets, ParamValues, TileId,
};
pub use evaluator::{Evaluator, RenderError};
pub use graph::{BuildError, Edge, Graph, GraphBuilder, NodeId, NodeIx, MAX_PAD};
pub use node::Node;
pub use port::{CoordSpace, PortKind, PortSpec};
pub use registry::{
    schema_frag, take_input_ref, take_optional_input_ref, BuiltNode, Connection, FactoryCtx,
    FactoryError, NodeFactory, NodeRegistry, StaticOp,
};

#[doc(hidden)]
pub use inventory;
pub use value::{PortValue, ScalarValue};

#[cfg(test)]
mod tests;
