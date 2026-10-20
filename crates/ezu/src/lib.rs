//! # ezu
//!
//! Painterly cartography — render vector tiles as paintings.
//!
//! This umbrella crate re-exports the sub-crates of the ezu workspace under a
//! single namespace, gated by feature flags.

pub use ezu_core as core;
pub use ezu_graph as graph;

#[cfg(feature = "features")]
pub use ezu_features as features;

#[cfg(feature = "paint")]
pub use ezu_paint as paint;

#[cfg(feature = "style-json")]
pub use ezu_style as style;

#[cfg(feature = "translate")]
pub use ezu_translate as translate;

// The module documents itself: an outer doc here would be joined to its
// inner one and resolve that one's intra-doc links from this crate root,
// where `Renderer` and its kin are not in scope.
#[cfg(feature = "renderer")]
pub mod renderer;
