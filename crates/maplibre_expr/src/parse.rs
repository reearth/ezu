//! Turning raw JSON (`serde_json::Value`) into an [`Expr`] tree.

use serde_json::Value as Json;

use crate::ast::{Expr, InterpKind, InterpSpace};
use crate::error::ParseError;
use crate::value::Value;

type Result<T> = std::result::Result<T, ParseError>;

/// Parse a MapLibre expression from JSON.
pub fn parse(json: &Json) -> Result<Expr> {
    match json {
        Json::Array(items) => parse_array(items),
        Json::Object(_) => Err(ParseError::new(
            "Expected an array, but found an object instead.",
        )),
        _ => Ok(Expr::Literal(Value::from_json(json))),
    }
}

fn parse_array(items: &[Json]) -> Result<Expr> {
    let first = items
        .first()
        .ok_or_else(|| ParseError::new("Expected an array with at least one element."))?;
    let op = first.as_str().ok_or_else(|| {
        ParseError::new("Expression name must be a string, but found a non-string instead.")
    })?;
    let args = &items[1..];

    match op {
        "literal" => {
            expect_arity(op, args, 1)?;
            Ok(Expr::Literal(Value::from_json(&args[0])))
        }
        "let" => parse_let(args),
        "var" => {
            expect_arity(op, args, 1)?;
            let name = args[0]
                .as_str()
                .ok_or_else(|| ParseError::new("'var' requires a string binding name."))?;
            Ok(Expr::Var(name.to_string()))
        }
        "match" => parse_match(args),
        "step" => parse_step(args),
        "interpolate" => parse_interpolate(InterpSpace::Rgb, args),
        "interpolate-hcl" => parse_interpolate(InterpSpace::Hcl, args),
        "interpolate-lab" => parse_interpolate(InterpSpace::Lab, args),
        _ => {
            check_generic_arity(op, args.len())?;
            let args = args.iter().map(parse).collect::<Result<Vec<_>>>()?;
            Ok(Expr::Call {
                op: op.to_string(),
                args,
            })
        }
    }
}

/// Reject unknown operators and calls with the wrong number of arguments at
/// parse time — these are `"result": "error"` cases in the spec fixtures.
///
/// Operators that MapLibre defines but this crate does not yet evaluate are
/// still accepted here (so their arguments parse); evaluation reports them as
/// unimplemented. Only genuinely unknown names are rejected.
fn check_generic_arity(op: &str, argc: usize) -> Result<()> {
    // `case` has an irregular (odd, >= 3) shape.
    if op == "case" {
        if argc < 3 || argc.is_multiple_of(2) {
            return Err(ParseError::new(
                "Expected an odd number of arguments (>= 3) to 'case'.",
            ));
        }
        return Ok(());
    }

    let range =
        arity(op).ok_or_else(|| ParseError::new(format!("Unknown expression name \"{op}\".")))?;
    let (min, max) = range;
    if argc < min || max.is_some_and(|m| argc > m) {
        return Err(ParseError::new(format!(
            "Wrong number of arguments to '{op}': expected {}, found {argc}.",
            match max {
                Some(m) if m == min => format!("{min}"),
                Some(m) => format!("{min}..={m}"),
                None => format!("at least {min}"),
            }
        )));
    }
    Ok(())
}

/// `(min, max)` argument counts for each known operator; `None` max means
/// variadic. Operators absent from this table are unknown names.
fn arity(op: &str) -> Option<(usize, Option<usize>)> {
    Some(match op {
        // lookups
        "get" => (1, Some(2)),
        "has" => (1, Some(2)),
        "properties"
        | "id"
        | "geometry-type"
        | "zoom"
        | "heatmap-density"
        | "line-progress"
        | "accumulated"
        | "e"
        | "pi"
        | "ln2"
        | "resolved-locale"
        | "raster-value"
        | "sky-radial-progress"
        | "measure-light"
        | "elevation" => (0, Some(0)),
        "at" => (2, Some(2)),
        "in" => (2, Some(2)),
        "index-of" => (2, Some(3)),
        "slice" => (2, Some(3)),
        "length" => (1, Some(1)),
        "feature-state" | "global-state" | "config" => (1, Some(2)),

        // decision / boolean
        "!" => (1, Some(1)),
        "all" | "any" | "coalesce" => (0, None),
        "error" => (1, Some(1)),
        "==" | "!=" | "<" | ">" | "<=" | ">=" => (2, Some(3)),

        // arithmetic — +/*/min/max accept zero args (identity element)
        "+" | "*" | "min" | "max" => (0, None),
        "-" => (1, Some(2)),
        "/" | "%" | "^" => (2, Some(2)),
        "abs" | "acos" | "asin" | "atan" | "ceil" | "cos" | "floor" | "ln" | "log10" | "log2"
        | "round" | "sin" | "sqrt" | "tan" => (1, Some(1)),
        "distance" => (1, Some(1)),

        // strings
        "concat" => (0, None),
        "upcase" | "downcase" => (1, Some(1)),
        "join" => (2, Some(2)),
        "split" => (2, Some(2)),
        "is-supported-script" => (1, Some(1)),
        "collator" => (1, Some(1)),
        "number-format" => (2, Some(2)),
        "format" | "image" => (1, None),

        // type assertions & conversions
        "array" => (1, Some(3)),
        "boolean" | "number" | "string" | "object" | "to-number" | "to-color" => (1, None),
        "to-boolean" | "to-string" | "to-rgba" | "typeof" => (1, Some(1)),

        // color constructors
        "rgb" => (3, Some(3)),
        "rgba" => (4, Some(4)),

        // geometry predicates
        "within" => (1, Some(1)),

        _ => return None,
    })
}

fn parse_let(args: &[Json]) -> Result<Expr> {
    if args.is_empty() || args.len().is_multiple_of(2) {
        return Err(ParseError::new(
            "Expected an odd number of arguments to 'let'.",
        ));
    }
    let mut bindings = Vec::new();
    let mut i = 0;
    while i + 1 < args.len() {
        let name = args[i]
            .as_str()
            .ok_or_else(|| ParseError::new("'let' binding names must be strings."))?;
        bindings.push((name.to_string(), parse(&args[i + 1])?));
        i += 2;
    }
    let body = parse(&args[args.len() - 1])?;
    Ok(Expr::Let {
        bindings,
        body: Box::new(body),
    })
}

fn parse_match(args: &[Json]) -> Result<Expr> {
    // args = input, (label, output)+, default  =>  even count, >= 4.
    if args.len() < 4 || !args.len().is_multiple_of(2) {
        return Err(ParseError::new(
            "Expected an even number of arguments (>= 4) to 'match'.",
        ));
    }
    let input = parse(&args[0])?;
    let mut arms = Vec::new();
    let mut i = 1;
    while i + 1 < args.len() {
        let labels = parse_match_labels(&args[i])?;
        let output = parse(&args[i + 1])?;
        arms.push((labels, output));
        i += 2;
    }
    let default = parse(&args[args.len() - 1])?;
    Ok(Expr::Match {
        input: Box::new(input),
        arms,
        default: Box::new(default),
    })
}

/// `match` labels are unquoted literals: a single value, or an array of values.
fn parse_match_labels(json: &Json) -> Result<Vec<Value>> {
    match json {
        Json::Array(items) => Ok(items.iter().map(Value::from_json).collect()),
        Json::Number(_) | Json::String(_) => Ok(vec![Value::from_json(json)]),
        _ => Err(ParseError::new(
            "Match labels must be numbers, strings, or arrays thereof.",
        )),
    }
}

fn parse_step(args: &[Json]) -> Result<Expr> {
    if args.len() < 3 || args.len() % 2 == 1 {
        return Err(ParseError::new(
            "Expected an even number of arguments (>= 4) to 'step'.",
        ));
    }
    let input = parse(&args[0])?;
    let output0 = parse(&args[1])?;
    let mut stops = Vec::new();
    let mut i = 2;
    while i + 1 < args.len() {
        let stop = args[i]
            .as_f64()
            .ok_or_else(|| ParseError::new("Step stop inputs must be numbers."))?;
        stops.push((stop, parse(&args[i + 1])?));
        i += 2;
    }
    check_ascending(&stops)?;
    Ok(Expr::Step {
        input: Box::new(input),
        output0: Box::new(output0),
        stops,
    })
}

fn parse_interpolate(space: InterpSpace, args: &[Json]) -> Result<Expr> {
    if args.len() < 4 || args.len() % 2 == 1 {
        return Err(ParseError::new(
            "Expected an even number of arguments (>= 4) to 'interpolate'.",
        ));
    }
    let kind = parse_interp_kind(&args[0])?;
    let input = parse(&args[1])?;
    let mut stops = Vec::new();
    let mut i = 2;
    while i + 1 < args.len() {
        let stop = args[i]
            .as_f64()
            .ok_or_else(|| ParseError::new("Interpolation stop inputs must be numbers."))?;
        stops.push((stop, parse(&args[i + 1])?));
        i += 2;
    }
    check_ascending(&stops)?;
    Ok(Expr::Interpolate {
        kind,
        space,
        input: Box::new(input),
        stops,
    })
}

fn parse_interp_kind(json: &Json) -> Result<InterpKind> {
    let items = json.as_array().ok_or_else(|| {
        ParseError::new("Interpolation type must be an array, e.g. [\"linear\"].")
    })?;
    let name = items
        .first()
        .and_then(Json::as_str)
        .ok_or_else(|| ParseError::new("Interpolation type name must be a string."))?;
    match name {
        "linear" => Ok(InterpKind::Linear),
        "exponential" => {
            let base = items
                .get(1)
                .and_then(Json::as_f64)
                .ok_or_else(|| ParseError::new("'exponential' interpolation requires a base."))?;
            Ok(InterpKind::Exponential(base))
        }
        "cubic-bezier" => {
            let n = |i: usize| items.get(i).and_then(Json::as_f64);
            match (n(1), n(2), n(3), n(4)) {
                (Some(a), Some(b), Some(c), Some(d)) => Ok(InterpKind::CubicBezier(a, b, c, d)),
                _ => Err(ParseError::new(
                    "'cubic-bezier' interpolation requires four control-point values.",
                )),
            }
        }
        other => Err(ParseError::new(format!(
            "Unknown interpolation type \"{other}\"."
        ))),
    }
}

fn check_ascending(stops: &[(f64, Expr)]) -> Result<()> {
    for pair in stops.windows(2) {
        if pair[1].0 <= pair[0].0 {
            return Err(ParseError::new(
                "Stop inputs must be arranged in strictly ascending order.",
            ));
        }
    }
    Ok(())
}

fn expect_arity(op: &str, args: &[Json], n: usize) -> Result<()> {
    if args.len() == n {
        Ok(())
    } else {
        Err(ParseError::new(format!(
            "Expected {n} argument(s) to '{op}', but found {}.",
            args.len()
        )))
    }
}
