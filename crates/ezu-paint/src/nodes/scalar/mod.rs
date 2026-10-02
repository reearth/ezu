//! Scalar-valued ops: compute `Scalar` numbers that other nodes
//! consume through `@node` references on their `In<T>` fields.
//!
//! - [`expr`] — a MapLibre expression evaluated per tile (zoom curves)
//! - [`math`] — arithmetic over numbers (literals, `$param`s, ports)
//! - [`zoom`] — the tile's zoom level as a number

mod expr;
// `field-math` applies the same functions per pixel, so it reads them
// from here rather than keeping a second copy that could drift.
pub(super) mod math;
mod zoom;
