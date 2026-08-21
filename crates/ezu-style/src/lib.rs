//! Ezu Style Spec — declarative style specification for painterly map
//! rendering.
//!
//! Parsing this crate produces a [`Document`] — a pure data structure,
//! not an evaluator. To execute it, feed the document to
//! `ezu-graph::build_graph` with a `NodeRegistry`.
//!
//! ```no_run
//! let json = std::fs::read_to_string("watercolor.json").unwrap();
//! let doc = ezu_style::Document::from_json(&json).unwrap();
//! println!("{} ({} nodes)", doc.name, doc.nodes.len());
//! ```

mod color;
mod expand;
mod jsonc;
mod spec;

pub use color::{parse_hex_color, parse_hex_color_u8};
pub use expand::{expand_functions, ExpandError, Expanded, KindCheck, MAX_EXPANDED_NODES};
pub use jsonc::{blank_comments, CommentError};
pub use spec::*;

#[derive(Debug, thiserror::Error)]
pub enum StyleError {
    #[error("style parse: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("style parse: {0}")]
    Comment(#[from] CommentError),
}
