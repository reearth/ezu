//! MapLibre layer filters → ezu.
//!
//! A layer's `filter` is routed by [`layer_filters`]:
//!
//! - **expression-form** filters (per [`is_expression_filter`]) pass through
//!   verbatim as a raw `filter-expr`, which ezu-paint evaluates via the
//!   `maplibre-expr` crate — full fidelity, including operators the
//!   structured form can't represent (`any`, `has`/`!has`, `<`/`>`,
//!   `geometry-type`/`$type`).
//! - **legacy-form** filters (and the bucket-membership filters synthesized
//!   for `match` fill colours) use the structured [`convert`] path below.
//!
//! The structured ezu filter is an AND-map: `{ key: value }`,
//! `{ key: [v1, v2] }` (membership), or `{ key: { "not": value|array } }`.
//! [`convert`] handles the comparison/membership operators in both spellings
//! MapLibre uses:
//!
//! - **legacy**: `["==", "kind", "park"]`, `["in", "kind", "a", "b"]`
//! - **expression**: `["==", ["get", "kind"], "park"]`,
//!   `["in", ["get", "kind"], ["literal", ["a", "b"]]]`
//!
//! plus `all` (AND) and `!` (negation of a single comparison). Operators
//! ezu's flat filter can't represent are reported and dropped rather than
//! silently mis-translated — but a genuine expression-form filter never
//! reaches [`convert`], so in practice these warnings are limited to the
//! legacy leftovers.

use serde_json::{Map, Value};

use crate::Report;

/// Route a layer's own `filter` to the right representation:
///
/// - **expression-form** (per [`is_expression_filter`]) → passed through
///   verbatim as a raw `filter-expr` (ezu-paint evaluates it via
///   `maplibre-expr` with full fidelity — no lossy structured translation,
///   no warning).
/// - **legacy-form** (or anything not recognized as an expression) → the
///   existing structured [`convert`] path.
///
/// Returns `(structured, expr)`; at most one is `Some`.
pub(crate) fn layer_filters(
    layer: &Map<String, Value>,
    report: &mut Report,
    id: &str,
) -> (Option<Map<String, Value>>, Option<Value>) {
    match layer.get("filter") {
        None => (None, None),
        Some(f) if is_expression_filter(f) => (None, Some(f.clone())),
        Some(f) => (convert(f, report, id), None),
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

/// Convert a MapLibre `filter` value into an ezu filter map. Unsupported
/// clauses are warned about (via `report`, tagged with `layer_id`) and
/// omitted. Returns `None` when nothing convertible remains.
pub fn convert(filter: &Value, report: &mut Report, layer_id: &str) -> Option<Map<String, Value>> {
    let mut out = Map::new();
    collect(filter, false, &mut out, report, layer_id);
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Fold one filter clause into `out`. `negate` inverts the sense (used by
/// the `!` operator) for the comparison/membership leaves.
fn collect(
    filter: &Value,
    negate: bool,
    out: &mut Map<String, Value>,
    report: &mut Report,
    layer_id: &str,
) {
    let Some(arr) = filter.as_array() else { return };
    let Some(op) = arr.first().and_then(Value::as_str) else {
        return;
    };
    match op {
        "all" if !negate => {
            for clause in &arr[1..] {
                collect(clause, false, out, report, layer_id);
            }
        }
        "!" => {
            // `["!", <clause>]` — invert a single inner comparison.
            if let Some(inner) = arr.get(1) {
                collect(inner, !negate, out, report, layer_id);
            }
        }
        "==" | "!=" => {
            let eq = (op == "==") ^ negate;
            match (key_arg(arr, 1), arr.get(2)) {
                (Some(key), Some(val)) if !is_special_key(&key) => {
                    let entry = if eq {
                        val.clone()
                    } else {
                        serde_json::json!({ "not": val })
                    };
                    out.insert(key, entry);
                }
                (Some(key), _) => warn_special(&key, report, layer_id),
                _ => warn_unsupported(op, report, layer_id),
            }
        }
        "in" | "!in" => {
            let is_in = (op == "in") ^ negate;
            match key_arg(arr, 1) {
                Some(key) if !is_special_key(&key) => {
                    let list = in_values(arr);
                    let entry = if is_in {
                        list
                    } else {
                        serde_json::json!({ "not": list })
                    };
                    out.insert(key, entry);
                }
                Some(key) => warn_special(&key, report, layer_id),
                None => warn_unsupported(op, report, layer_id),
            }
        }
        other => warn_unsupported(other, report, layer_id),
    }
}

/// The property name a comparison keys on — either a bare string (legacy)
/// or an `["get", "<name>"]` expression.
fn key_arg(arr: &[Value], i: usize) -> Option<String> {
    let a = arr.get(i)?;
    if let Some(s) = a.as_str() {
        return Some(s.to_string());
    }
    let inner = a.as_array()?;
    if inner.first()?.as_str()? == "get" {
        return inner.get(1)?.as_str().map(str::to_string);
    }
    None
}

/// The membership list for `in`/`!in` — legacy is the trailing args, the
/// expression form is a single `["literal", [ ... ]]`.
fn in_values(arr: &[Value]) -> Value {
    if let Some(lit) = arr.get(2).and_then(Value::as_array) {
        if lit.first().and_then(Value::as_str) == Some("literal") {
            if let Some(items) = lit.get(1).and_then(Value::as_array) {
                return Value::Array(items.clone());
            }
        }
    }
    Value::Array(arr[2..].to_vec())
}

/// MapLibre special keys (`$type`, `$id`) aren't plain feature properties;
/// ezu filters match on properties only.
fn is_special_key(key: &str) -> bool {
    key.starts_with('$')
}

fn warn_special(key: &str, report: &mut Report, layer_id: &str) {
    report.warn(format!(
        "layer `{layer_id}`: filter on special key `{key}` not supported — ignored"
    ));
}

fn warn_unsupported(op: &str, report: &mut Report, layer_id: &str) {
    report.warn(format!(
        "layer `{layer_id}`: filter operator `{op}` not supported — ignored"
    ));
}
