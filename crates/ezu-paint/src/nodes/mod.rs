//! Node implementations for the graph evaluator. One file per op.
//!
//! Ops are grouped into category submodules:
//!
//! - [`raster`] — raster utility ops (`solid`, `circle`, `blur`,
//!   `blend`)
//! - [`source`] — feature sources, either bound by the host (MVT,
//!   GeoJSON, …) or synthesized (`features`, `literal-geometry`,
//!   `tile-bounds`, `point-grid`)
//! - [`paint`] — paint features onto a canvas (`fill-solid`,
//!   `fill-dabs`, `line`, `stamp`, `brush-file`)
//! - [`brush`] — `() -> Brush` sources: load a brush asset
//!   (`brush-file`) or synthesize one (`brush-solid`)
//! - [`geometry`] — `Features -> Features` transforms (`centroid`,
//!   `boundary`, `simplify`, `convex-hull`, `buffer`, `hatch`,
//!   `dash`, `wave`)
//!
//! Each op file ends in `ezu_graph::submit_node!(...Factory)` which
//! registers it with the global inventory. [`default_registry`] simply
//! collects everything that's been submitted — adding a new op means
//! creating a file and submitting it; no edits here required.
//!
//! The `features` node resolves tile-scoped layers through the
//! unified [`AssetLoader`](ezu_graph::AssetLoader) under reserved
//! `tile.<layer>` names — like sampling a shader uniform. The host
//! (e.g. the `tokyo` example) decodes its MVT/GeoJSON input into
//! [`ezu_features::FeatureLayer`] and binds each layer before rendering.
//!
//! Shared helpers (parameter parsing, color conversion, Canvas
//! plumbing, payload downcasting) live in [`common`].

use ezu_graph::NodeRegistry;

mod brush;
mod common;
mod geometry;
mod paint;
mod raster;
mod source;

pub use common::{BrushPayload, FilteredFeatures};

/// Build a registry of every built-in op, collected from
/// `ezu_graph::submit_node!` submissions across the linked crates.
pub fn default_registry() -> NodeRegistry {
    NodeRegistry::from_inventory()
}
