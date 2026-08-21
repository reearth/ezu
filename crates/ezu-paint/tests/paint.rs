//! Every `ezu-paint` integration test, in one binary.
//!
//! Cargo builds and links one test binary per `.rs` file directly under
//! `tests/`, which meant 35 compile-and-link cycles for what is really one
//! suite — most of a local `cargo test` was linking. The tests themselves live
//! in `tests/paint/` (a subdirectory is not auto-discovered as a target) and
//! are pulled in here as modules, so the suite links once.
//!
//! The paths are explicit because a crate-root `mod foo;` looks for
//! `tests/foo.rs`, not `tests/paint/foo.rs`. Adding a test file means dropping
//! it in `tests/paint/` and adding a line below; it reaches the shared helpers
//! through `crate::common`.

#[path = "paint/common/mod.rs"]
mod common;

#[path = "paint/color.rs"]
mod color;
#[path = "paint/color_ramp.rs"]
mod color_ramp;
#[path = "paint/compositing.rs"]
mod compositing;
#[path = "paint/contour.rs"]
mod contour;
#[path = "paint/data_driven_circle.rs"]
mod data_driven_circle;
#[path = "paint/data_driven_fill.rs"]
mod data_driven_fill;
#[path = "paint/data_driven_icon.rs"]
mod data_driven_icon;
#[path = "paint/data_driven_stamp.rs"]
mod data_driven_stamp;
#[path = "paint/data_driven_stroke.rs"]
mod data_driven_stroke;
#[path = "paint/density.rs"]
mod density;
#[path = "paint/dot_density.rs"]
mod dot_density;
#[path = "paint/feature_sources.rs"]
mod feature_sources;
#[path = "paint/functions.rs"]
mod functions;
#[path = "paint/generator_kinds.rs"]
mod generator_kinds;
#[path = "paint/geometry_ops.rs"]
mod geometry_ops;
#[path = "paint/gradient.rs"]
mod gradient;
#[path = "paint/graticule.rs"]
mod graticule;
#[path = "paint/icon_labels.rs"]
mod icon_labels;
#[path = "paint/label_placement.rs"]
mod label_placement;
#[path = "paint/legend_swatch.rs"]
mod legend_swatch;
#[path = "paint/morphology.rs"]
mod morphology;
#[path = "paint/noise_warp.rs"]
mod noise_warp;
#[path = "paint/params.rs"]
mod params;
#[path = "paint/pixel_filters.rs"]
mod pixel_filters;
#[path = "paint/raster_layout.rs"]
mod raster_layout;
#[path = "paint/raster_source.rs"]
mod raster_source;
#[path = "paint/rect_canvas.rs"]
mod rect_canvas;
#[path = "paint/scalar_ops.rs"]
mod scalar_ops;
#[path = "paint/schema.rs"]
mod schema;
#[path = "paint/stroke_gap_width.rs"]
mod stroke_gap_width;
#[path = "paint/text_collision.rs"]
mod text_collision;
#[path = "paint/text_labels.rs"]
mod text_labels;
#[path = "paint/text_line_labels.rs"]
mod text_line_labels;
#[path = "paint/text_padding_expr.rs"]
mod text_padding_expr;
#[path = "paint/text_variable_anchor.rs"]
mod text_variable_anchor;
#[path = "paint/utility_ops.rs"]
mod utility_ops;
#[path = "paint/vector_paint.rs"]
mod vector_paint;
#[path = "paint/voronoi_ops.rs"]
mod voronoi_ops;
