//! Pure-function geometry operations on the crate's owned types
//! ([`Polygon`](super::Polygon), `LineString` as `Vec<(i32, i32)>`,
//! `Point` as `(i32, i32)`).
//!
//! Ops are decoupled from the graph layer: they take and return plain
//! data, so they can be called from nodes, the CLI, or tests without
//! pulling in `ezu-graph`. Node wrappers live in `ezu-paint/src/nodes/`.
//!
//! Coordinates are tile-local integer pixels matching the upstream
//! decoder output. Internally each op promotes to `f64` as needed and
//! rounds back on the way out.

pub mod boundary;
pub mod buffer;
pub mod centroid;
pub mod convert;
pub mod convex_hull;
pub mod hatch;
pub mod simplify;
pub mod voronoi;
