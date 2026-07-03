//! Scalar-valued ops: compute `Scalar` numbers that other nodes
//! consume through `@node` references on their `In<T>` fields.
//!
//! - [`expr`] — a MapLibre expression evaluated per tile (zoom curves)
//! - [`math`] — arithmetic over numbers (literals, `$param`s, ports)
//! - [`zoom`] — the tile's zoom level as a number

mod expr;
mod math;
mod zoom;
