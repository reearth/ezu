//! Crate-internal unit tests. Split by concern:
//!
//! - [`topology`] — `GraphBuilder` topo / type-check / pad propagation
//! - [`build`] — JSON `Document` → typed graph via `build_graph`
//! - [`eval`] — `Evaluator` + cache reuse
//!
//! Shared mock nodes live in [`common`].

mod build;
mod common;
mod eval;
mod topology;
