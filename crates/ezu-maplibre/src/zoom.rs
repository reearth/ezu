//! Evaluate zoom-dependent MapLibre property functions at a single zoom.
//!
//! ezu renders one integer zoom per tile, so we bake a zoom function to a
//! constant at the target zoom (from [`ConvertOptions::zoom`]). Handles:
//! - plain literals (number / colour string),
//! - legacy `{ "stops": [[z, v], ...], "base"? }` functions,
//! - `["interpolate", <interp>, ["zoom"], z0, v0, ...]` expressions,
//! - `["step", ["zoom"], v0, z1, v1, ...]` expressions.
//!
//! When no target zoom is given, the base value (first stop) is used.

use ezu_core::color::{interpolate, InterpSpace};
use serde_json::Value;

use crate::color::{parse_rgba, rgba_to_hex};

/// Resolve a numeric property to `f64` at `zoom`.
pub fn number_at(v: &Value, zoom: Option<f64>) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Object(_) => stops_at(v, zoom, interp_num).and_then(|v| v.as_f64()),
        Value::Array(_) => expr_at(v, zoom, interp_num).and_then(|v| v.as_f64()),
        _ => None,
    }
}

/// Resolve a colour property to a colour string at `zoom`. Colours are
/// interpolated between the enclosing stops in the space the MapLibre
/// operator selects: `interpolate` (and legacy stops) → RGB,
/// `interpolate-hcl` → HCL, `interpolate-lab` → LAB. `step` picks the lower
/// stop. Matches MapLibre's colour maths (shared `ezu-core` implementation).
pub fn color_at(v: &Value, zoom: Option<f64>) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(_) => {
            stops_at(v, zoom, color_interp(InterpSpace::Rgb)).and_then(as_color_string)
        }
        Value::Array(_) => {
            let space = color_space_of(v);
            expr_at(v, zoom, color_interp(space)).and_then(as_color_string)
        }
        _ => None,
    }
}

/// The colour space a MapLibre interpolation expression selects.
fn color_space_of(v: &Value) -> InterpSpace {
    match v
        .as_array()
        .and_then(|a| a.first())
        .and_then(|x| x.as_str())
    {
        Some("interpolate-hcl") => InterpSpace::Hcl,
        Some("interpolate-lab") => InterpSpace::Lab,
        _ => InterpSpace::Rgb,
    }
}

/// Build a stop interpolator that blends two colour-string `Value`s in
/// `space` and returns the baked `#hex`. Falls back to the lower stop if a
/// colour doesn't parse (e.g. a data-driven sub-expression).
fn color_interp(space: InterpSpace) -> impl Fn(&Value, &Value, f64) -> Value {
    move |a, b, t| match (
        a.as_str().and_then(parse_rgba),
        b.as_str().and_then(parse_rgba),
    ) {
        (Some(from), Some(to)) => {
            Value::String(rgba_to_hex(interpolate(from, to, t as f32, space)))
        }
        _ => a.clone(),
    }
}

fn as_color_string(v: Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s),
        _ => None,
    }
}

/// Interpolate two numeric `Value`s (returned as a `Value::Number`).
fn interp_num(a: &Value, b: &Value, t: f64) -> Value {
    match (a.as_f64(), b.as_f64()) {
        (Some(a), Some(b)) => serde_json::json!(a + (b - a) * t),
        _ => a.clone(),
    }
}

/// Legacy `{ "stops": [[z, v], ...] }` (optionally with `"base"` for
/// exponential interpolation).
fn stops_at(
    v: &Value,
    zoom: Option<f64>,
    interp: impl Fn(&Value, &Value, f64) -> Value,
) -> Option<Value> {
    let obj = v.as_object()?;
    let stops = obj.get("stops")?.as_array()?;
    if stops.is_empty() {
        return None;
    }
    let base = obj.get("base").and_then(Value::as_f64).unwrap_or(1.0);
    let pairs: Vec<(f64, &Value)> = stops
        .iter()
        .filter_map(|s| {
            let s = s.as_array()?;
            Some((s.first()?.as_f64()?, s.get(1)?))
        })
        .collect();
    Some(sample(&pairs, zoom, base, interp))
}

/// `["interpolate", <interp>, ["zoom"], z0, v0, z1, v1, ...]` or
/// `["step", ["zoom"], v0, z1, v1, ...]`.
fn expr_at(
    v: &Value,
    zoom: Option<f64>,
    interp: impl Fn(&Value, &Value, f64) -> Value,
) -> Option<Value> {
    let arr = v.as_array()?;
    match arr.first()?.as_str()? {
        "interpolate" | "interpolate-hcl" | "interpolate-lab" => {
            // arr[1] = interpolation type (["linear"] / ["exponential", b] / ["cubic-bezier",..])
            let base = match arr.get(1).and_then(Value::as_array) {
                Some(t) if t.first().and_then(Value::as_str) == Some("exponential") => {
                    t.get(1).and_then(Value::as_f64).unwrap_or(1.0)
                }
                _ => 1.0,
            };
            // arr[2] must be ["zoom"]; otherwise it's data-driven → bail.
            let input = arr.get(2)?.as_array()?;
            if input.first()?.as_str()? != "zoom" {
                return None;
            }
            let mut pairs: Vec<(f64, &Value)> = Vec::new();
            let mut i = 3;
            while i + 1 < arr.len() {
                let z = arr[i].as_f64()?;
                pairs.push((z, &arr[i + 1]));
                i += 2;
            }
            Some(sample(&pairs, zoom, base, interp))
        }
        "step" => {
            let input = arr.get(1)?.as_array()?;
            if input.first()?.as_str()? != "zoom" {
                return None;
            }
            // v0, then (z, v) pairs.
            let v0 = arr.get(2)?;
            let Some(z) = zoom else {
                return Some(v0.clone());
            };
            let mut chosen = v0;
            let mut i = 3;
            while i + 1 < arr.len() {
                let stop_z = arr[i].as_f64()?;
                if z >= stop_z {
                    chosen = &arr[i + 1];
                }
                i += 2;
            }
            Some(chosen.clone())
        }
        _ => None,
    }
}

/// Piecewise sampling shared by legacy stops and `interpolate`. `base` is
/// the exponential interpolation base (1.0 = linear).
fn sample(
    pairs: &[(f64, &Value)],
    zoom: Option<f64>,
    base: f64,
    interp: impl Fn(&Value, &Value, f64) -> Value,
) -> Value {
    if pairs.is_empty() {
        return Value::Null;
    }
    let Some(z) = zoom else {
        return pairs[0].1.clone();
    };
    if z <= pairs[0].0 {
        return pairs[0].1.clone();
    }
    if z >= pairs[pairs.len() - 1].0 {
        return pairs[pairs.len() - 1].1.clone();
    }
    for w in pairs.windows(2) {
        let (z0, v0) = w[0];
        let (z1, v1) = w[1];
        if z >= z0 && z <= z1 {
            let t = if (z1 - z0).abs() < f64::EPSILON {
                0.0
            } else if (base - 1.0).abs() < f64::EPSILON {
                (z - z0) / (z1 - z0) // linear
            } else {
                // exponential interpolation factor (MapLibre spec).
                (base.powf(z - z0) - 1.0) / (base.powf(z1 - z0) - 1.0)
            };
            return interp(v0, v1, t);
        }
    }
    pairs[0].1.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(op: &str, zoom: f64) -> Option<String> {
        // A two-stop zoom colour ramp red@0 -> blue@10.
        let v = serde_json::json!([op, ["linear"], ["zoom"], 0, "#ff0000", 10, "#0000ff"]);
        color_at(&v, Some(zoom))
    }

    #[test]
    fn colours_interpolate_and_space_matters() {
        assert_eq!(ramp("interpolate", 0.0).as_deref(), Some("#ff0000"));
        assert_eq!(ramp("interpolate", 10.0).as_deref(), Some("#0000ff"));
        let rgb_mid = ramp("interpolate", 5.0).unwrap();
        // RGB midpoint of #ff0000..#0000ff is #800080 — a real blend, not a step.
        assert_eq!(rgb_mid, "#800080");
        let hcl_mid = ramp("interpolate-hcl", 5.0).unwrap();
        let lab_mid = ramp("interpolate-lab", 5.0).unwrap();
        assert_ne!(rgb_mid, hcl_mid, "hcl should differ from rgb");
        assert_ne!(rgb_mid, lab_mid, "lab should differ from rgb");
    }

    #[test]
    fn step_picks_the_lower_stop() {
        let v = serde_json::json!(["step", ["zoom"], "#ff0000", 5, "#00ff00"]);
        assert_eq!(color_at(&v, Some(3.0)).as_deref(), Some("#ff0000"));
        assert_eq!(color_at(&v, Some(7.0)).as_deref(), Some("#00ff00"));
    }
}
