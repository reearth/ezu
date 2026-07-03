//! Translate map-engine styles into **ezu recipes** — the node-DAG
//! [`Document`](https://docs.rs/ezu-style) JSON that ezu renders on the CPU.
//!
//! Map engines describe their maps in their own style languages. `ezu-translate`
//! lowers those styles into ezu recipes so they can be rendered by ezu.
//!
//! [MapLibre GL] is the first frontend, exposed under the [`maplibre`] module
//! (its public API is [`maplibre::convert`], [`maplibre::ConvertOptions`],
//! [`maplibre::Report`], and [`maplibre::ConvertError`]). More engines can be
//! added over time as sibling modules alongside it.
//!
//! [MapLibre GL]: https://maplibre.org/maplibre-style-spec/

pub mod maplibre;
