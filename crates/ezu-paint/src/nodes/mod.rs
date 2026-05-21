//! Node implementations for the graph evaluator. One file per op.
//!
//! Ops are grouped into category submodules:
//!
//! - [`raster`] — raster / mask utility ops (`solid`, `mask-*`,
//!   `fill-with-mask`, `blend`)
//! - [`source`] — feature sources, either fed by the host (MVT) or
//!   synthesized (`mvt-source`, `literal-geometry`, `tile-bounds`,
//!   `point-grid`)
//! - [`paint`] — paint features onto a canvas (`fill-solid`,
//!   `fill-dabs`, `line`, `brush-file`)
//! - [`geometry`] — `Features -> Features` transforms (`centroid`,
//!   `boundary`, `simplify`, `convex-hull`, `buffer`, `hatch`)
//!
//! Each op file ends in `ezu_graph::submit_node!(...Factory)` which
//! registers it with the global inventory. [`default_registry`] simply
//! collects everything that's been submitted — adding a new op means
//! creating a file and submitting it; no edits here required.
//!
//! MVT-driven nodes downcast `EvalCtx::tile_data` to
//! `Arc<ezu_features::mvt::DecodedTile>`. The host (e.g. the `tokyo` example)
//! fetches and decodes the tile and passes it via
//! [`Evaluator::render_with_tile_data`](ezu_graph::Evaluator).
//!
//! Shared helpers (parameter parsing, color conversion, Canvas
//! plumbing, payload downcasting) live in [`common`].

use ezu_graph::NodeRegistry;

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
