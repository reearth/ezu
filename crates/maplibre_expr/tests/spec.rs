//! Conformance harness: runs the vendored `maplibre-style-spec` expression
//! fixtures, one libtest case per fixture directory.
//!
//! Each `test.json` carries an `expression`, a list of `inputs`, and the
//! `expected` compile result plus per-input `outputs`. For every fixture we:
//!
//!   1. parse the expression (checking success vs. compile-error), then
//!   2. evaluate it against each input and compare to the expected output,
//!      matching `{ "error": ... }` outputs against evaluation errors.
//!
//! Fixtures listed in `tests/known_failures.txt` are reported as *ignored*
//! rather than failing — that file is the running to-do list of operators and
//! behaviours not yet implemented. Nothing is skipped silently: the count of
//! ignored fixtures is printed by the runner and the list is under version
//! control.
//!
//! ## Scope note
//!
//! This harness verifies `compiled.result` (success vs. error) and the
//! per-input `outputs`. It does **not** yet assert the static-analysis fields
//! (`type`, `isFeatureConstant`, `isZoomConstant`); adding a type-inference
//! pass is future work.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use libtest_mimic::{Arguments, Failed, Trial};
use maplibre_expr::{evaluate, parse, EvaluationContext, Feature, Value};
use serde_json::Value as Json;

fn main() {
    let args = Arguments::from_args();

    let root = fixtures_root();
    let known = load_known_failures();
    let mut fixtures = Vec::new();
    collect(&root, &root, &mut fixtures);
    fixtures.sort();

    let trials = fixtures
        .into_iter()
        .map(|(name, path)| {
            let ignored = known.contains(&name);
            let mut trial = Trial::test(name, move || run_fixture(&path));
            if ignored {
                trial = trial.with_ignored_flag(true);
            }
            trial
        })
        .collect();

    libtest_mimic::run(&args, trials).exit();
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("expression")
}

fn load_known_failures() -> HashSet<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("known_failures.txt");
    let Ok(contents) = fs::read_to_string(path) else {
        return HashSet::new();
    };
    contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

/// Recursively find every `test.json`, naming each by its path relative to the
/// fixtures root (e.g. `interpolate/linear`).
fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if path.file_name().and_then(|n| n.to_str()) == Some("test.json") {
            let name = path
                .parent()
                .unwrap()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((name, path));
        }
    }
}

fn run_fixture(path: &Path) -> Result<(), Failed> {
    let raw = fs::read_to_string(path).map_err(|e| format!("cannot read fixture: {e}"))?;
    let doc: Json = serde_json::from_str(&raw).map_err(|e| format!("invalid fixture json: {e}"))?;

    let expression = doc
        .get("expression")
        .ok_or("fixture missing \"expression\"")?;
    let expected = doc.get("expected").ok_or("fixture missing \"expected\"")?;
    let compiled_result = expected
        .get("compiled")
        .and_then(|c| c.get("result"))
        .and_then(Json::as_str)
        .unwrap_or("success");

    let parsed = parse(expression);

    if compiled_result == "error" {
        return match parsed {
            Err(_) => Ok(()),
            Ok(_) => Err("expected a compile error, but the expression parsed successfully".into()),
        };
    }

    let expr =
        parsed.map_err(|e| format!("expected successful compile, but parsing failed: {e}"))?;

    let empty = Vec::new();
    let inputs = doc.get("inputs").and_then(Json::as_array).unwrap_or(&empty);
    let outputs = expected
        .get("outputs")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();

    for (i, input) in inputs.iter().enumerate() {
        let ctx = build_context(input)?;
        let expected_output = outputs
            .get(i)
            .ok_or_else(|| format!("input #{i} has no expected output"))?;

        match evaluate(&expr, &ctx) {
            Ok(value) => {
                if let Some(err_obj) = expected_output.get("error") {
                    return Err(format!(
                        "input #{i}: expected evaluation error ({err_obj}), got value {:?}",
                        value
                    )
                    .into());
                }
                let actual = value_to_json(&value);
                if !json_close(&actual, expected_output) {
                    return Err(
                        format!("input #{i}: expected {expected_output}, got {actual}").into(),
                    );
                }
            }
            Err(e) => {
                if expected_output.get("error").is_none() {
                    return Err(format!(
                        "input #{i}: expected {expected_output}, got evaluation error: {e}"
                    )
                    .into());
                }
            }
        }
    }

    Ok(())
}

/// Build an [`EvaluationContext`] from a fixture input `[globals, feature]`.
fn build_context(input: &Json) -> Result<EvaluationContext, Failed> {
    let items = input
        .as_array()
        .ok_or("each input must be a [globals, feature] array")?;

    let mut ctx = EvaluationContext::new();
    if let Some(zoom) = items
        .first()
        .and_then(|g| g.get("zoom"))
        .and_then(Json::as_f64)
    {
        ctx.zoom = Some(zoom);
    }

    if let Some(feature_json) = items.get(1) {
        ctx.feature = build_feature(feature_json);
    }
    Ok(ctx)
}

fn build_feature(json: &Json) -> Feature {
    let mut feature = Feature::default();

    if let Some(props) = json.get("properties").and_then(Json::as_object) {
        feature.properties = props
            .iter()
            .map(|(k, v)| (k.clone(), Value::from_json(v)))
            .collect::<BTreeMap<_, _>>();
    }
    if let Some(id) = json.get("id") {
        if !id.is_null() {
            feature.id = Some(Value::from_json(id));
        }
    }
    feature.geometry_type = geometry_type(json);
    feature
}

fn geometry_type(json: &Json) -> Option<String> {
    if let Some(t) = json
        .get("geometry")
        .and_then(|g| g.get("type"))
        .and_then(Json::as_str)
    {
        return Some(t.to_string());
    }
    match json.get("type") {
        Some(Json::String(s)) => Some(s.clone()),
        Some(Json::Number(n)) => match n.as_u64() {
            Some(1) => Some("Point".into()),
            Some(2) => Some("LineString".into()),
            Some(3) => Some("Polygon".into()),
            _ => None,
        },
        _ => None,
    }
}

/// Serialize a [`Value`] to JSON using the same representation the spec
/// fixtures use for expected outputs.
fn value_to_json(value: &Value) -> Json {
    match value {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Number(n) => serde_json::json!(n),
        Value::String(s) => Json::String(s.clone()),
        // Colors are compared as normalized [r, g, b, a] arrays.
        Value::Color(c) => Json::Array(
            c.to_rgba_unit()
                .iter()
                .map(|n| serde_json::json!(n))
                .collect(),
        ),
        Value::Array(a) => Json::Array(a.iter().map(value_to_json).collect()),
        Value::Object(o) => Json::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect(),
        ),
    }
}

/// Structural JSON equality that mirrors the upstream harness: numbers are
/// reduced to 6 significant decimal figures (see [`strip_precision`]) before
/// being compared, matching `deepEqual`/`stripPrecision` in the spec repo's
/// `test/lib/json-diff.ts`.
fn json_close(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => {
            let (x, y) = (
                x.as_f64().unwrap_or(f64::NAN),
                y.as_f64().unwrap_or(f64::NAN),
            );
            if x.is_nan() || y.is_nan() {
                return x.is_nan() && y.is_nan();
            }
            let (sx, sy) = (strip_precision(x, 6), strip_precision(y, 6));
            (sx - sy).abs() <= 1e-9 * sx.abs().max(1.0)
        }
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| json_close(a, b))
        }
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| json_close(v, w)))
        }
        _ => a == b,
    }
}

/// Reduce `x` to `sig` significant decimal figures by truncation, matching
/// upstream `stripPrecision`. The double-floor guards against a value that is
/// already stripped drifting under floating-point rounding.
fn strip_precision(x: f64, sig: i32) -> f64 {
    if x == 0.0 {
        return 0.0;
    }
    let multiplier = 10f64.powf((sig as f64 - x.abs().log10().ceil()).max(0.0));
    let first = (x * multiplier).floor() / multiplier;
    (first * multiplier).floor() / multiplier
}
