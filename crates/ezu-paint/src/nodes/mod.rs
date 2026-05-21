//! Node implementations for the graph evaluator. One file per op.
//!
//! Built-in op set:
//!
//! - Sources / utility (no MVT): [`solid`], [`mask_solid`], [`mask_circle`],
//!   [`mask_blur`], [`fill_with_mask`], [`blend`]
//! - MVT-driven: [`mvt_source`], [`fill_solid`], [`fill_dabs`], [`line`],
//!   [`brush_file`]
//!
//! MVT-driven nodes downcast `EvalCtx::tile_data` to
//! `Arc<ezu_features::mvt::DecodedTile>`. The host (e.g. the `tokyo` example)
//! fetches and decodes the tile and passes it via
//! [`Evaluator::render_with_tile_data`](ezu_graph::Evaluator).
//!
//! Shared helpers (parameter parsing, color conversion, Canvas
//! plumbing, payload downcasting) live in [`common`].

use ezu_graph::NodeRegistry;

mod blend;
mod boundary;
mod brush_file;
mod buffer;
mod centroid;
mod common;
mod convex_hull;
mod fill_dabs;
mod hatch;
mod simplify;
mod fill_solid;
mod fill_with_mask;
mod line;
mod mask_blur;
mod mask_circle;
mod mask_solid;
mod mvt_source;
mod solid;

pub use common::{BrushPayload, FilteredFeatures};

/// Build a registry with all built-in ops registered.
pub fn default_registry() -> NodeRegistry {
    let mut r = NodeRegistry::new();
    // raster / mask utility
    r.register("solid", solid::SolidFactory);
    r.register("mask-solid", mask_solid::MaskSolidFactory);
    r.register("mask-circle", mask_circle::MaskCircleFactory);
    r.register("mask-blur", mask_blur::MaskBlurFactory);
    r.register("fill-with-mask", fill_with_mask::FillWithMaskFactory);
    r.register("blend", blend::BlendFactory);
    // mvt-driven
    r.register("mvt-source", mvt_source::MvtSourceFactory);
    r.register("fill-solid", fill_solid::FillSolidFactory);
    r.register("fill-dabs", fill_dabs::FillDabsFactory);
    r.register("line", line::LineFactory);
    r.register("brush-file", brush_file::BrushFileFactory);
    // geometry ops
    r.register("centroid", centroid::CentroidFactory);
    r.register("boundary", boundary::BoundaryFactory);
    r.register("simplify", simplify::SimplifyFactory);
    r.register("convex-hull", convex_hull::ConvexHullFactory);
    r.register("buffer", buffer::BufferFactory);
    r.register("hatch", hatch::HatchFactory);
    r
}
