//! MapLibre layer filters → ezu.
//!
//! A layer's `filter` is routed by [`layer_filter_expr`]:
//!
//! - **expression-form** filters (per [`is_expression_filter`]) pass through
//!   verbatim as a raw `filter-expr`, which ezu-paint evaluates via the
//!   `maplibre-expr` crate — full fidelity, including `any`, `has`/`!has`,
//!   `<`/`>`, `geometry-type`/`$type`, etc.
//! - **legacy-form** filters are intentionally unsupported: they are
//!   vanishingly rare in modern styles, and legacy compatibility belongs in
//!   `maplibre-expr` if ever needed. Such a layer is reported and left
//!   unfiltered.

use serde_json::Value;

use crate::maplibre::Report;

/// The raw expression filter to attach to a layer's features node, if any.
///
/// - **expression-form** (per [`is_expression_filter`]) → the filter passed
///   through verbatim so ezu-paint can evaluate it via `maplibre-expr`.
/// - **legacy-form** (anything not recognized as an expression) → `None` plus
///   a warning; the layer is left unfiltered.
pub(crate) fn layer_filter_expr(
    layer: &serde_json::Map<String, Value>,
    report: &mut Report,
    id: &str,
) -> Option<Value> {
    match layer.get("filter") {
        None => None,
        Some(f) if is_expression_filter(f) => Some(f.clone()),
        Some(_) => {
            report.warn(format!(
                "layer `{id}`: legacy filter form is not supported (rare; use expression syntax) — layer left unfiltered"
            ));
            None
        }
    }
}

/// Whether a MapLibre `filter` is an *expression*, as opposed to a *legacy*
/// filter. Ported faithfully from MapLibre's `isExpressionFilter`: this is
/// exactly how MapLibre decides whether to feed a filter to the expression
/// evaluator or the legacy filter compiler.
pub(crate) fn is_expression_filter(f: &Value) -> bool {
    // booleans are valid expressions
    if f.is_boolean() {
        return true;
    }
    let Some(arr) = f.as_array() else {
        return false;
    };
    if arr.is_empty() {
        return false;
    }
    let op = arr[0].as_str().unwrap_or("");
    match op {
        "has" => {
            arr.len() >= 2
                && arr
                    .get(1)
                    .and_then(|v| v.as_str())
                    .is_none_or(|s| s != "$id" && s != "$type")
        }
        "in" => arr.len() >= 3 && (!arr[1].is_string() || arr.get(2).is_some_and(|v| v.is_array())),
        "!in" | "!has" | "none" => false, // legacy-only
        "==" | "!=" | ">" | ">=" | "<" | "<=" => {
            arr.len() != 3 || arr[1].is_array() || arr[2].is_array()
        }
        "any" | "all" => arr[1..]
            .iter()
            .all(|sub| sub.is_boolean() || is_expression_filter(sub)),
        _ => true, // any other first element ⇒ an expression
    }
}
