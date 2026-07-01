# maplibre_expr

A pure-Rust parser and evaluator for [MapLibre GL style expressions][spec].

This crate is **private** (`publish = false`) and has **no dependency on ezu**
— it only turns a MapLibre expression (JSON) into a typed tree and evaluates it.

```rust
use maplibre_expr::{parse, evaluate, EvaluationContext, Feature, Value};
use std::collections::BTreeMap;

let expr = parse(&serde_json::json!(["*", ["get", "x"], 2])).unwrap();

let mut props = BTreeMap::new();
props.insert("x".to_string(), Value::Number(21.0));
let ctx = EvaluationContext::new().with_feature(Feature { properties: props, ..Default::default() });

assert_eq!(evaluate(&expr, &ctx).unwrap(), Value::Number(42.0));
```

## Layout

| Module        | Responsibility                                              |
| ------------- | ----------------------------------------------------------- |
| `value.rs`    | `Value` — the expression type system (+ number formatting)  |
| `color.rs`    | `Color` and a CSS color parser (hex, `rgb()/hsl()`, named)  |
| `context.rs`  | `EvaluationContext` (zoom + `Feature`)                      |
| `ast.rs`      | `Expr` — the parsed tree; special forms for let/match/step/interpolate |
| `parse.rs`    | JSON → `Expr`, with operator/arity validation               |
| `eval.rs`     | Evaluating an `Expr` against a context                       |

## Conformance testing

The crate is validated against a **vendored snapshot** of the upstream
[`maplibre-style-spec`][spec] expression fixtures (`tests/fixtures/expression`,
see `ATTRIBUTION.md`). The harness in `tests/spec.rs` turns **each fixture
directory into one libtest case** (via `libtest-mimic`), so a run reads like:

```
cargo test -p maplibre_expr --test spec
# test result: ok. 286 passed; 0 failed; 277 ignored; ...
```

For every fixture it parses the `expression` (checking success vs. compile
error), then evaluates it against each `input` and compares to the expected
`output`, matching `{ "error": ... }` outputs against evaluation errors.
Numbers are compared with the same 6-significant-figure `stripPrecision` rule
the upstream suite uses.

**Scope note:** the harness verifies `compiled.result` (success/error) and the
per-input `outputs`. It does not yet assert the static-analysis fields
(`type`, `isFeatureConstant`, `isZoomConstant`) — a type-inference pass is
future work.

### The skip-list is the roadmap

Fixtures that don't pass yet are listed in `tests/known_failures.txt` and
reported as **ignored** rather than failing the build. That file is grouped by
*reason* (unimplemented operators, HCL/LAB color spaces, compile-time type
validation, type-context coercion, legacy function syntax), so it doubles as
the to-do list. Nothing is skipped silently.

To make progress: implement a behaviour, delete the corresponding line(s) from
`known_failures.txt`, and the fixtures graduate to enforced tests.

### Refreshing the fixtures

```sh
tests/refresh_fixtures.sh [git-ref]   # re-vendors and prints the new commit
```

[spec]: https://maplibre.org/maplibre-style-spec/expressions/
