//! Error types for parsing and evaluation.

use thiserror::Error;

/// An error raised while turning JSON into an [`Expr`](crate::Expr).
///
/// This corresponds to a `"result": "error"` compile outcome in the spec
/// fixtures — the expression is malformed and cannot be built at all.
#[derive(Debug, Clone, Error, PartialEq)]
#[error("{message}")]
pub struct ParseError {
    pub message: String,
}

impl ParseError {
    pub fn new(message: impl Into<String>) -> ParseError {
        ParseError {
            message: message.into(),
        }
    }
}

/// An error raised while evaluating a well-formed expression against an input.
///
/// This corresponds to a per-input `{ "error": ... }` output in the spec
/// fixtures — the expression compiled fine but blew up on this feature.
#[derive(Debug, Clone, Error, PartialEq)]
#[error("{message}")]
pub struct EvalError {
    pub message: String,
}

impl EvalError {
    pub fn new(message: impl Into<String>) -> EvalError {
        EvalError {
            message: message.into(),
        }
    }
}
