//! # ezu
//!
//! Painterly cartography — render vector tiles as paintings.
//!
//! This umbrella crate re-exports the sub-crates of the ezu workspace under a
//! single namespace, gated by feature flags.

pub use ezu_core as core;
pub use ezu_graph as graph;

#[cfg(feature = "mvt")]
pub use ezu_mvt as mvt;

#[cfg(feature = "pmtiles")]
pub use ezu_pmtiles as pmtiles;

#[cfg(feature = "paint")]
pub use ezu_paint as paint;

#[cfg(feature = "style-json")]
pub use ezu_style as style;
