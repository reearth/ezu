//! Typed DAG evaluator for the Ezu Style Spec.
//!
//! - [`port`] — `PortKind`, `PortSpec`, `CoordSpace`
//! - [`buf`] / [`value`] — concrete buffers and the `PortValue` enum
//! - [`node`] — the `Node` trait every operation implements
//! - [`graph`] — `GraphBuilder` / `Graph` with topo sort, type checking,
//!   cycle detection, and pad propagation
//! - [`registry`] / [`build`] — turn a parsed `ezu_style::Document`
//!   into a typed `Graph` using a registry of node factories
//! - [`input`] — `In<T>` scalar fields: literal / `$param` / `@node` port
//! - [`eval`] / [`cache`] / [`evaluator`] — render a tile
//! - [`mem`] — opt-in accounting of live intermediate pixel buffers

pub mod buf;
pub mod build;
pub mod cache;
pub mod eval;
pub mod evaluator;
pub mod graph;
pub mod input;
pub mod mem;
pub mod neighbor;
pub mod node;
pub mod port;
pub mod registry;
pub mod value;

pub use buf::{GeoScale, OpaqueValue, RasterBuf, ScalarField, SpriteRect, SpriteSheet};
pub use build::{build_graph, BuildGraphError};
pub use cache::{Cache, CacheKey, Hash128};
pub use eval::{
    Asset, AssetError, AssetLoader, CanvasInfo, EvalCtx, EvalError, NoAssets, ParamValues, TileId,
};
pub use evaluator::{Evaluator, RenderError};
pub use graph::{BuildError, Edge, Graph, GraphBuilder, NodeId, NodeIx, MAX_PAD};
pub use input::{parse_param_value, In, InParts, InReader, PaddingIn, ScalarType, ACCEPTS_SCALAR};
pub use neighbor::{neighbor_binding, neighbor_bindings, parse_neighbor_binding};
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
