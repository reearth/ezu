//! MapLibre filter expressions → ezu feature filter objects.
//!
//! ezu filters are an AND-map: `{ key: value }`, `{ key: [v1, v2] }`
//! (membership), or `{ key: { "not": value|array } }`. We convert the
//! comparison/membership operators in both spellings MapLibre uses:
//!
//! - **legacy**: `["==", "kind", "park"]`, `["in", "kind", "a", "b"]`
//! - **expression**: `["==", ["get", "kind"], "park"]`,
//!   `["in", ["get", "kind"], ["literal", ["a", "b"]]]`
//!
//! plus `all` (AND) and `!` (negation of a single comparison). Operators
//! ezu's flat filter can't represent — `any` (OR), `has`/`!has` (field
//! existence), `<`/`>`, `geometry-type`/`$type` — are reported and dropped
//! rather than silently mis-translated.

use serde_json::{Map, Value};

use crate::Report;

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
