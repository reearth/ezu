//! MapLibre filter expressions → ezu feature filter objects.
//!
//! ezu filters are an AND-map: `{ key: value }`, `{ key: [v1, v2] }`
//! (membership), or `{ key: { "not": value|array } }`. That covers the
//! MapLibre legacy operators `all` / `==` / `!=` / `in` / `!in`. Anything
//! else (comparisons, `has`, `any`, `geometry-type`, `get`-expressions) is
//! reported and dropped rather than silently mis-translated.

use serde_json::{Map, Value};

use crate::Report;

/// Convert a MapLibre `filter` value into an ezu filter map. Unsupported
/// clauses are warned about (via `report`, tagged with `layer_id`) and
/// omitted. Returns `None` when nothing convertible remains.
pub fn convert(filter: &Value, report: &mut Report, layer_id: &str) -> Option<Map<String, Value>> {
    let mut out = Map::new();
    collect(filter, &mut out, report, layer_id);
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn collect(filter: &Value, out: &mut Map<String, Value>, report: &mut Report, layer_id: &str) {
    let Some(arr) = filter.as_array() else { return };
    let Some(op) = arr.first().and_then(Value::as_str) else {
        return;
    };
    match op {
        "all" => {
            for clause in &arr[1..] {
                collect(clause, out, report, layer_id);
            }
        }
        "==" => {
            if let (Some(key), Some(val)) = (str_arg(arr, 1), arr.get(2)) {
                if is_special_key(&key) {
                    warn_special(&key, report, layer_id);
                } else {
                    out.insert(key, val.clone());
                }
            }
        }
        "!=" => {
            if let (Some(key), Some(val)) = (str_arg(arr, 1), arr.get(2)) {
                if is_special_key(&key) {
                    warn_special(&key, report, layer_id);
                } else {
                    out.insert(key, serde_json::json!({ "not": val }));
                }
            }
        }
        "in" => {
            if let Some(key) = str_arg(arr, 1) {
                if is_special_key(&key) {
                    warn_special(&key, report, layer_id);
                } else {
                    out.insert(key, Value::Array(arr[2..].to_vec()));
                }
            }
        }
        "!in" => {
            if let Some(key) = str_arg(arr, 1) {
                if is_special_key(&key) {
                    warn_special(&key, report, layer_id);
                } else {
                    out.insert(
                        key,
                        serde_json::json!({ "not": Value::Array(arr[2..].to_vec()) }),
                    );
                }
            }
        }
        other => report.warn(format!(
            "layer `{layer_id}`: filter operator `{other}` not supported — ignored"
        )),
    }
}

fn str_arg(arr: &[Value], i: usize) -> Option<String> {
    arr.get(i).and_then(Value::as_str).map(str::to_string)
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
