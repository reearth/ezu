//! Evaluating a parsed [`Expr`] against an [`EvaluationContext`].

use crate::ast::{Expr, InterpKind, InterpSpace};
use crate::color::Color;
use crate::context::EvaluationContext;
use crate::error::EvalError;
use crate::value::Value;

type Result<T> = std::result::Result<T, EvalError>;

/// Evaluate an expression against a context, returning its value.
pub fn eval(expr: &Expr, ctx: &EvaluationContext) -> Result<Value> {
    let mut ev = Evaluator {
        ctx,
        scope: Vec::new(),
    };
    ev.eval(expr)
}

struct Evaluator<'a> {
    ctx: &'a EvaluationContext,
    scope: Vec<(String, Value)>,
}

impl Evaluator<'_> {
    fn eval(&mut self, expr: &Expr) -> Result<Value> {
        match expr {
            Expr::Literal(v) => Ok(v.clone()),
            Expr::Var(name) => self
                .scope
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| EvalError::new(format!("Unknown variable \"{name}\"."))),
            Expr::Let { bindings, body } => self.eval_let(bindings, body),
            Expr::Match {
                input,
                arms,
                default,
            } => self.eval_match(input, arms, default),
            Expr::Step {
                input,
                output0,
                stops,
            } => self.eval_step(input, output0, stops),
            Expr::Interpolate {
                kind,
                space,
                input,
                stops,
            } => self.eval_interpolate(*kind, *space, input, stops),
            Expr::Call { op, args } => self.eval_call(op, args),
        }
    }

    fn eval_number(&mut self, expr: &Expr) -> Result<f64> {
        match self.eval(expr)? {
            Value::Number(n) => Ok(n),
            other => Err(type_error("number", &other)),
        }
    }

    fn eval_let(&mut self, bindings: &[(String, Expr)], body: &Expr) -> Result<Value> {
        let base = self.scope.len();
        for (name, value_expr) in bindings {
            let v = self.eval(value_expr)?;
            self.scope.push((name.clone(), v));
        }
        let result = self.eval(body);
        self.scope.truncate(base);
        result
    }

    fn eval_match(
        &mut self,
        input: &Expr,
        arms: &[(Vec<Value>, Expr)],
        default: &Expr,
    ) -> Result<Value> {
        let subject = self.eval(input)?;
        for (labels, output) in arms {
            if labels.iter().any(|l| values_equal(l, &subject)) {
                return self.eval(output);
            }
        }
        self.eval(default)
    }

    fn eval_step(&mut self, input: &Expr, output0: &Expr, stops: &[(f64, Expr)]) -> Result<Value> {
        let x = self.eval_number(input)?;
        let mut chosen = output0;
        for (stop, output) in stops {
            if x >= *stop {
                chosen = output;
            } else {
                break;
            }
        }
        self.eval(chosen)
    }

    fn eval_interpolate(
        &mut self,
        kind: InterpKind,
        space: InterpSpace,
        input: &Expr,
        stops: &[(f64, Expr)],
    ) -> Result<Value> {
        let x = self.eval_number(input)?;
        // Below the first / above the last stop: clamp to the endpoint.
        if x <= stops[0].0 {
            return self.eval_interp_output(&stops[0].1);
        }
        if x >= stops[stops.len() - 1].0 {
            return self.eval_interp_output(&stops[stops.len() - 1].1);
        }
        // Find the bracketing pair.
        let mut idx = 0;
        for i in 0..stops.len() - 1 {
            if x >= stops[i].0 && x < stops[i + 1].0 {
                idx = i;
                break;
            }
        }
        let (lo, hi) = (stops[idx].0, stops[idx + 1].0);
        let lo_v = self.eval_interp_output(&stops[idx].1)?;
        let hi_v = self.eval_interp_output(&stops[idx + 1].1)?;
        let t = interpolation_factor(kind, x, lo, hi);
        interpolate_values(&lo_v, &hi_v, t, space)
    }

    /// Evaluate an interpolation stop output, coercing bare color strings
    /// (e.g. `"red"`, `"#f00"`) to colors as MapLibre does when the output
    /// type is `color`.
    fn eval_interp_output(&mut self, expr: &Expr) -> Result<Value> {
        // Interpolation outputs can only be numbers, colors, or number arrays,
        // so a bare string output is always a color to be parsed.
        match self.eval(expr)? {
            Value::String(s) => match Color::parse(&s) {
                Some(c) => Ok(Value::Color(c)),
                None => Err(EvalError::new(format!(
                    "Could not parse color from value '{s}'"
                ))),
            },
            other => Ok(other),
        }
    }

    fn eval_call(&mut self, op: &str, args: &[Expr]) -> Result<Value> {
        match op {
            // --- feature / object lookups ---
            "get" => self.op_get(args),
            "has" => self.op_has(args),
            "properties" => Ok(Value::Object(self.ctx.feature.properties.clone())),
            "id" => Ok(self.ctx.feature.id.clone().unwrap_or(Value::Null)),
            "geometry-type" => Ok(self
                .ctx
                .feature
                .geometry_type
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null)),
            "zoom" => self
                .ctx
                .zoom
                .map(Value::Number)
                .ok_or_else(|| EvalError::new("The 'zoom' expression is unavailable here.")),

            // --- collections ---
            "at" => self.op_at(args),
            "in" => self.op_in(args),
            "index-of" => self.op_index_of(args),
            "length" => self.op_length(args),
            "slice" => self.op_slice(args),

            // --- decisions / booleans ---
            "!" => Ok(Value::Bool(!self.eval(&args[0])?.is_truthy())),
            "all" => self.op_all(args),
            "any" => self.op_any(args),
            "case" => self.op_case(args),
            "coalesce" => self.op_coalesce(args),
            "==" => self.op_eq(args, true),
            "!=" => self.op_eq(args, false),
            "<" => self.op_cmp(args, Ordering::Lt),
            ">" => self.op_cmp(args, Ordering::Gt),
            "<=" => self.op_cmp(args, Ordering::Le),
            ">=" => self.op_cmp(args, Ordering::Ge),

            // --- arithmetic ---
            "+" => self.fold_num(args, 0.0, |a, b| a + b),
            "*" => self.fold_num(args, 1.0, |a, b| a * b),
            "-" => self.op_minus(args),
            "/" => {
                let a = self.eval_number(&args[0])?;
                let b = self.eval_number(&args[1])?;
                Ok(Value::Number(a / b))
            }
            "%" => {
                let a = self.eval_number(&args[0])?;
                let b = self.eval_number(&args[1])?;
                Ok(Value::Number(a % b))
            }
            "^" => {
                let a = self.eval_number(&args[0])?;
                let b = self.eval_number(&args[1])?;
                Ok(Value::Number(a.powf(b)))
            }
            "abs" => self.map_num(args, f64::abs),
            "ceil" => self.map_num(args, f64::ceil),
            "floor" => self.map_num(args, f64::floor),
            "round" => self.map_num(args, f64::round),
            "sqrt" => self.map_num(args, f64::sqrt),
            "sin" => self.map_num(args, f64::sin),
            "cos" => self.map_num(args, f64::cos),
            "tan" => self.map_num(args, f64::tan),
            "asin" => self.map_num(args, f64::asin),
            "acos" => self.map_num(args, f64::acos),
            "atan" => self.map_num(args, f64::atan),
            "ln" => self.map_num(args, f64::ln),
            "log2" => self.map_num(args, f64::log2),
            "log10" => self.map_num(args, f64::log10),
            "min" => self.fold_num(args, f64::INFINITY, f64::min),
            "max" => self.fold_num(args, f64::NEG_INFINITY, f64::max),
            "error" => Err(EvalError::new(self.eval_string(&args[0])?)),
            "e" => Ok(Value::Number(std::f64::consts::E)),
            "pi" => Ok(Value::Number(std::f64::consts::PI)),
            "ln2" => Ok(Value::Number(std::f64::consts::LN_2)),

            // --- strings ---
            "concat" => self.op_concat(args),
            "upcase" => Ok(Value::String(self.eval_string(&args[0])?.to_uppercase())),
            "downcase" => Ok(Value::String(self.eval_string(&args[0])?.to_lowercase())),
            "join" => self.op_join(args),
            "split" => self.op_split(args),

            // --- type assertions & conversions ---
            "array" => self.op_array(args),
            "boolean" => self.assert_type(args, "boolean", |v| matches!(v, Value::Bool(_))),
            "number" => self.assert_type(args, "number", |v| matches!(v, Value::Number(_))),
            "string" => self.assert_type(args, "string", |v| matches!(v, Value::String(_))),
            "object" => self.assert_type(args, "object", |v| matches!(v, Value::Object(_))),
            "to-boolean" => Ok(Value::Bool(self.eval(&args[0])?.is_truthy())),
            "to-number" => self.op_to_number(args),
            "to-string" => Ok(Value::String(to_string_value(&self.eval(&args[0])?))),
            "to-color" => self.op_to_color(args),
            "to-rgba" => self.op_to_rgba(args),
            "rgb" => self.op_rgb(args, false),
            "rgba" => self.op_rgb(args, true),

            other => Err(EvalError::new(format!(
                "Unimplemented operator \"{other}\"."
            ))),
        }
    }

    // ---- lookups ------------------------------------------------------

    fn op_get(&mut self, args: &[Expr]) -> Result<Value> {
        let key = self.eval_string(&args[0])?;
        if args.len() >= 2 {
            match self.eval(&args[1])? {
                Value::Object(o) => Ok(o.get(&key).cloned().unwrap_or(Value::Null)),
                other => Err(type_error("object", &other)),
            }
        } else {
            Ok(self
                .ctx
                .feature
                .properties
                .get(&key)
                .cloned()
                .unwrap_or(Value::Null))
        }
    }

    fn op_has(&mut self, args: &[Expr]) -> Result<Value> {
        let key = self.eval_string(&args[0])?;
        let present = if args.len() >= 2 {
            match self.eval(&args[1])? {
                Value::Object(o) => o.contains_key(&key),
                other => return Err(type_error("object", &other)),
            }
        } else {
            self.ctx.feature.properties.contains_key(&key)
        };
        Ok(Value::Bool(present))
    }

    fn op_at(&mut self, args: &[Expr]) -> Result<Value> {
        let index = self.eval_number(&args[0])?;
        let array = match self.eval(&args[1])? {
            Value::Array(a) => a,
            other => return Err(type_error("array", &other)),
        };
        if index < 0.0 || index != index.trunc() || index as usize >= array.len() {
            return Err(EvalError::new(format!(
                "Array index out of bounds: {} > {}.",
                index,
                array.len().saturating_sub(1)
            )));
        }
        Ok(array[index as usize].clone())
    }

    fn op_in(&mut self, args: &[Expr]) -> Result<Value> {
        let needle = self.eval(&args[0])?;
        let haystack = self.eval(&args[1])?;
        // Mirrors MapLibre: a falsy haystack (null, empty string) is a miss
        // before any type checking kicks in.
        if !haystack.is_truthy() {
            return Ok(Value::Bool(false));
        }
        require_searchable_needle(&needle)?;
        let found = match &haystack {
            Value::String(s) => s.contains(&js_string(&needle)),
            Value::Array(a) => a.iter().any(|v| values_equal(v, &needle)),
            other => return Err(type_error("array or string", other)),
        };
        Ok(Value::Bool(found))
    }

    fn op_index_of(&mut self, args: &[Expr]) -> Result<Value> {
        let needle = self.eval(&args[0])?;
        require_searchable_needle(&needle)?;
        let haystack = self.eval(&args[1])?;
        let from = if args.len() >= 3 {
            Some(self.eval_number(&args[2])?)
        } else {
            None
        };
        match &haystack {
            Value::String(s) => Ok(Value::Number(str_index_of(s, &js_string(&needle), from))),
            Value::Array(a) => Ok(Value::Number(array_index_of(a, &needle, from))),
            other => Err(type_error("array or string", other)),
        }
    }

    fn op_length(&mut self, args: &[Expr]) -> Result<Value> {
        match self.eval(&args[0])? {
            Value::String(s) => Ok(Value::Number(s.chars().count() as f64)),
            Value::Array(a) => Ok(Value::Number(a.len() as f64)),
            other => Err(type_error("string or array", &other)),
        }
    }

    fn op_slice(&mut self, args: &[Expr]) -> Result<Value> {
        let value = self.eval(&args[0])?;
        let begin = self.eval_number(&args[1])?;
        let end = if args.len() >= 3 {
            Some(self.eval_number(&args[2])?)
        } else {
            None
        };
        match value {
            Value::Array(a) => {
                let (s, e) = js_slice_bounds(begin, end, a.len());
                Ok(Value::Array(a[s..e].to_vec()))
            }
            Value::String(s) => {
                let chars: Vec<char> = s.chars().collect();
                let (a, b) = js_slice_bounds(begin, end, chars.len());
                Ok(Value::String(chars[a..b].iter().collect()))
            }
            other => Err(type_error("string or array", &other)),
        }
    }

    // ---- decisions ----------------------------------------------------

    fn op_all(&mut self, args: &[Expr]) -> Result<Value> {
        for a in args {
            if !self.eval(a)?.is_truthy() {
                return Ok(Value::Bool(false));
            }
        }
        Ok(Value::Bool(true))
    }

    fn op_any(&mut self, args: &[Expr]) -> Result<Value> {
        for a in args {
            if self.eval(a)?.is_truthy() {
                return Ok(Value::Bool(true));
            }
        }
        Ok(Value::Bool(false))
    }

    fn op_case(&mut self, args: &[Expr]) -> Result<Value> {
        let mut i = 0;
        while i + 1 < args.len() {
            match self.eval(&args[i])? {
                Value::Bool(true) => return self.eval(&args[i + 1]),
                Value::Bool(false) => {}
                other => return Err(type_error("boolean", &other)),
            }
            i += 2;
        }
        self.eval(&args[args.len() - 1])
    }

    fn op_coalesce(&mut self, args: &[Expr]) -> Result<Value> {
        let mut last_err = None;
        for a in args {
            match self.eval(a) {
                Ok(Value::Null) => continue,
                Ok(v) => return Ok(v),
                Err(e) => last_err = Some(e),
            }
        }
        match last_err {
            Some(_) => Ok(Value::Null),
            None => Ok(Value::Null),
        }
    }

    fn op_eq(&mut self, args: &[Expr], want_equal: bool) -> Result<Value> {
        let a = self.eval(&args[0])?;
        let b = self.eval(&args[1])?;
        Ok(Value::Bool(values_equal(&a, &b) == want_equal))
    }

    fn op_cmp(&mut self, args: &[Expr], ord: Ordering) -> Result<Value> {
        let a = self.eval(&args[0])?;
        let b = self.eval(&args[1])?;
        let result = match (&a, &b) {
            (Value::Number(x), Value::Number(y)) => ord.test(x.partial_cmp(y)),
            (Value::String(x), Value::String(y)) => ord.test(Some(x.cmp(y))),
            _ => {
                return Err(EvalError::new(format!(
                    "Cannot compare {} and {}.",
                    a.type_name(),
                    b.type_name()
                )))
            }
        };
        Ok(Value::Bool(result))
    }

    // ---- arithmetic helpers ------------------------------------------

    fn fold_num(&mut self, args: &[Expr], init: f64, f: fn(f64, f64) -> f64) -> Result<Value> {
        let mut acc = init;
        for a in args {
            acc = f(acc, self.eval_number(a)?);
        }
        Ok(Value::Number(acc))
    }

    fn map_num(&mut self, args: &[Expr], f: fn(f64) -> f64) -> Result<Value> {
        Ok(Value::Number(f(self.eval_number(&args[0])?)))
    }

    fn op_minus(&mut self, args: &[Expr]) -> Result<Value> {
        let a = self.eval_number(&args[0])?;
        if args.len() == 1 {
            Ok(Value::Number(-a))
        } else {
            Ok(Value::Number(a - self.eval_number(&args[1])?))
        }
    }

    // ---- strings ------------------------------------------------------

    fn eval_string(&mut self, expr: &Expr) -> Result<String> {
        match self.eval(expr)? {
            Value::String(s) => Ok(s),
            other => Err(type_error("string", &other)),
        }
    }

    fn op_concat(&mut self, args: &[Expr]) -> Result<Value> {
        let mut out = String::new();
        for a in args {
            out.push_str(&to_string_value(&self.eval(a)?));
        }
        Ok(Value::String(out))
    }

    fn op_join(&mut self, args: &[Expr]) -> Result<Value> {
        let array = match self.eval(&args[0])? {
            Value::Array(a) => a,
            other => return Err(type_error("array", &other)),
        };
        let sep = self.eval_string(&args[1])?;
        let parts: Vec<String> = array.iter().map(to_string_value).collect();
        Ok(Value::String(parts.join(&sep)))
    }

    fn op_split(&mut self, args: &[Expr]) -> Result<Value> {
        let s = self.eval_string(&args[0])?;
        let sep = self.eval_string(&args[1])?;
        let parts: Vec<Value> = if sep.is_empty() {
            s.chars().map(|c| Value::String(c.to_string())).collect()
        } else {
            s.split(&sep)
                .map(|p| Value::String(p.to_string()))
                .collect()
        };
        Ok(Value::Array(parts))
    }

    // ---- type assertions & conversions -------------------------------

    fn assert_type(
        &mut self,
        args: &[Expr],
        name: &str,
        pred: fn(&Value) -> bool,
    ) -> Result<Value> {
        let mut last = Value::Null;
        for a in args {
            last = self.eval(a)?;
            if pred(&last) {
                return Ok(last);
            }
        }
        Err(type_error(name, &last))
    }

    fn op_array(&mut self, args: &[Expr]) -> Result<Value> {
        // ["array", value] | ["array", type, value] | ["array", type, N, value]
        let value = self.eval(&args[args.len() - 1])?;
        let array = match value {
            Value::Array(a) => a,
            other => return Err(type_error("array", &other)),
        };
        if args.len() == 1 {
            return Ok(Value::Array(array));
        }
        let item_type = self.eval_string(&args[0])?;
        if args.len() >= 3 {
            let n = self.eval_number(&args[1])? as usize;
            if array.len() != n {
                return Err(type_error(
                    &format!("array<{item_type}, {n}>"),
                    &Value::Array(array),
                ));
            }
        }
        let matches = |v: &Value| match item_type.as_str() {
            "string" => matches!(v, Value::String(_)),
            "number" => matches!(v, Value::Number(_)),
            "boolean" => matches!(v, Value::Bool(_)),
            _ => true,
        };
        if !array.iter().all(matches) {
            return Err(type_error(
                &format!("array<{item_type}>"),
                &Value::Array(array),
            ));
        }
        Ok(Value::Array(array))
    }

    fn op_to_number(&mut self, args: &[Expr]) -> Result<Value> {
        let mut last = Value::Null;
        for a in args {
            last = self.eval(a)?;
            match &last {
                Value::Number(n) => return Ok(Value::Number(*n)),
                Value::Null => return Ok(Value::Number(0.0)),
                Value::Bool(b) => return Ok(Value::Number(if *b { 1.0 } else { 0.0 })),
                Value::String(s) => {
                    let trimmed = s.trim();
                    if trimmed.is_empty() {
                        return Ok(Value::Number(0.0));
                    }
                    if let Ok(n) = trimmed.parse::<f64>() {
                        return Ok(Value::Number(n));
                    }
                }
                _ => {}
            }
        }
        Err(EvalError::new(format!(
            "Could not convert {} to number.",
            last.type_name()
        )))
    }

    fn op_to_color(&mut self, args: &[Expr]) -> Result<Value> {
        let mut last = Value::Null;
        for a in args {
            last = self.eval(a)?;
            if let Some(c) = coerce_color(&last) {
                return Ok(Value::Color(c));
            }
        }
        Err(EvalError::new(format!(
            "Could not parse {} as a color.",
            last.type_name()
        )))
    }

    fn op_to_rgba(&mut self, args: &[Expr]) -> Result<Value> {
        let value = self.eval(&args[0])?;
        // The argument type is Color; MapLibre coerces strings/arrays here.
        let color = coerce_color(&value).ok_or_else(|| type_error("color", &value))?;
        let [r, g, b, a] = color.to_rgba255();
        Ok(Value::Array(vec![
            Value::Number(r),
            Value::Number(g),
            Value::Number(b),
            Value::Number(a),
        ]))
    }

    fn op_rgb(&mut self, args: &[Expr], with_alpha: bool) -> Result<Value> {
        let r = self.eval_number(&args[0])?;
        let g = self.eval_number(&args[1])?;
        let b = self.eval_number(&args[2])?;
        let a = if with_alpha {
            self.eval_number(&args[3])?
        } else {
            1.0
        };
        for (chan, v) in [("red", r), ("green", g), ("blue", b)] {
            if !(0.0..=255.0).contains(&v) {
                return Err(EvalError::new(format!(
                    "Invalid {chan} component {v}: expected a number between 0 and 255."
                )));
            }
        }
        if !(0.0..=1.0).contains(&a) {
            return Err(EvalError::new(format!(
                "Invalid alpha component {a}: expected a number between 0 and 1."
            )));
        }
        Ok(Value::Color(Color::from_rgba8(r, g, b, a)))
    }
}

// ---- free helpers -----------------------------------------------------

#[derive(Clone, Copy)]
enum Ordering {
    Lt,
    Gt,
    Le,
    Ge,
}

impl Ordering {
    fn test(self, ord: Option<std::cmp::Ordering>) -> bool {
        use std::cmp::Ordering as O;
        match ord {
            None => false,
            Some(o) => match self {
                Ordering::Lt => o == O::Less,
                Ordering::Gt => o == O::Greater,
                Ordering::Le => o != O::Greater,
                Ordering::Ge => o != O::Less,
            },
        }
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x == y,
        _ => a == b,
    }
}

fn type_error(expected: &str, found: &Value) -> EvalError {
    EvalError::new(format!(
        "Expected value to be of type {expected}, but found {} instead.",
        found.type_name()
    ))
}

/// `in` / `index-of` accept only primitive needles.
fn require_searchable_needle(needle: &Value) -> Result<()> {
    match needle {
        Value::Bool(_) | Value::String(_) | Value::Number(_) | Value::Null => Ok(()),
        other => Err(EvalError::new(format!(
            "Expected first argument to be of type boolean, string, number or null, but found {} instead.",
            other.type_name()
        ))),
    }
}

/// How a value stringifies when searched inside a string haystack, matching
/// JavaScript's coercion in `String.prototype.indexOf` (`null` -> `"null"`).
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => crate::value::format_number(*n),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `String.prototype.indexOf` over code points: negative `from` clamps to 0.
fn str_index_of(hay: &str, needle: &str, from: Option<f64>) -> f64 {
    let hay: Vec<char> = hay.chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    let start = from.map_or(0, |f| {
        if f < 0.0 {
            0
        } else {
            (f as usize).min(hay.len())
        }
    });
    if needle.is_empty() {
        return start.min(hay.len()) as f64;
    }
    if needle.len() > hay.len() {
        return -1.0;
    }
    for i in start..=hay.len() - needle.len() {
        if hay[i..i + needle.len()] == needle[..] {
            return i as f64;
        }
    }
    -1.0
}

/// `Array.prototype.indexOf`: negative `from` counts back from the end.
fn array_index_of(arr: &[Value], needle: &Value, from: Option<f64>) -> f64 {
    let len = arr.len();
    let start = match from {
        Some(f) if f < 0.0 => (len as f64 + f).max(0.0) as usize,
        Some(f) => (f as usize).min(len),
        None => 0,
    };
    arr.iter()
        .enumerate()
        .skip(start)
        .find(|(_, v)| values_equal(v, needle))
        .map_or(-1.0, |(i, _)| i as f64)
}

/// Normalize `slice(begin, end)` indices the way JavaScript's `slice` does:
/// negatives count from the end, everything clamps to `0..=len`, and an empty
/// range yields `start == end`.
fn js_slice_bounds(begin: f64, end: Option<f64>, len: usize) -> (usize, usize) {
    let clamp = |i: f64| -> usize {
        if i < 0.0 {
            (len as f64 + i).max(0.0) as usize
        } else {
            (i as usize).min(len)
        }
    };
    let start = clamp(begin);
    let stop = end.map_or(len, clamp);
    (start, stop.max(start))
}

/// Coerce a value to a [`Color`]: pass colors through, parse CSS strings, and
/// read `[r, g, b]` / `[r, g, b, a]` numeric arrays (channels in `0..=255`).
fn coerce_color(v: &Value) -> Option<Color> {
    match v {
        Value::Color(c) => Some(*c),
        Value::String(s) => Color::parse(s),
        Value::Array(a) if a.len() == 3 || a.len() == 4 => {
            let n = |i: usize| a.get(i).and_then(Value::as_number);
            match (n(0), n(1), n(2)) {
                (Some(r), Some(g), Some(b)) => {
                    Some(Color::from_rgba8(r, g, b, n(3).unwrap_or(1.0)))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// MapLibre's `toString`: `null` -> `""`, scalars via their native string form,
/// colors as `rgba(...)`, and arrays/objects as compact JSON.
fn to_string_value(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => crate::value::format_number(*n),
        Value::String(s) => s.clone(),
        Value::Color(c) => c.to_string(),
        Value::Array(_) | Value::Object(_) => json_string(v),
    }
}

/// Compact JSON serialization for `to-string`/`concat` of arrays and objects.
fn json_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => crate::value::format_number(*n),
        Value::String(s) => format!("{s:?}"),
        Value::Color(c) => format!("{:?}", c.to_string()),
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().map(json_string).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(o) => {
            let parts: Vec<String> = o
                .iter()
                .map(|(k, val)| format!("{k:?}:{}", json_string(val)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

fn interpolation_factor(kind: InterpKind, x: f64, lo: f64, hi: f64) -> f64 {
    let span = hi - lo;
    match kind {
        InterpKind::Linear => (x - lo) / span,
        InterpKind::Exponential(base) => {
            if (base - 1.0).abs() < f64::EPSILON {
                (x - lo) / span
            } else {
                (base.powf(x - lo) - 1.0) / (base.powf(span) - 1.0)
            }
        }
        InterpKind::CubicBezier(x1, y1, x2, y2) => {
            let t = (x - lo) / span;
            unit_bezier(x1, y1, x2, y2, t)
        }
    }
}

fn interpolate_values(lo: &Value, hi: &Value, t: f64, space: InterpSpace) -> Result<Value> {
    match (lo, hi) {
        (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a + (b - a) * t)),
        (Value::Color(a), Value::Color(b)) => Ok(Value::Color(interpolate_color(*a, *b, t, space))),
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            let mut out = Vec::with_capacity(a.len());
            for (x, y) in a.iter().zip(b) {
                out.push(interpolate_values(x, y, t, space)?);
            }
            Ok(Value::Array(out))
        }
        _ => Err(EvalError::new(
            "Interpolation outputs must be numbers, colors, or arrays of numbers.",
        )),
    }
}

fn interpolate_color(a: Color, b: Color, t: f64, _space: InterpSpace) -> Color {
    // Component-wise RGBA interpolation. HCL/LAB spaces fall back to this and
    // are covered by the fixture skip-list until proper conversion lands.
    Color::new(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
        a.a + (b.a - a.a) * t,
    )
}

/// Solve a unit cubic Bézier easing curve for `y` at parameter `x` (both in
/// `0..=1`), matching MapLibre's `UnitBezier` implementation.
fn unit_bezier(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    let cx = 3.0 * x1;
    let bx = 3.0 * (x2 - x1) - cx;
    let ax = 1.0 - cx - bx;
    let cy = 3.0 * y1;
    let by = 3.0 * (y2 - y1) - cy;
    let ay = 1.0 - cy - by;

    let sample_x = |t: f64| ((ax * t + bx) * t + cx) * t;
    let sample_y = |t: f64| ((ay * t + by) * t + cy) * t;
    let sample_dx = |t: f64| (3.0 * ax * t + 2.0 * bx) * t + cx;

    // Newton-Raphson, then bisection fallback.
    let mut t = x;
    for _ in 0..8 {
        let x2 = sample_x(t) - x;
        if x2.abs() < 1e-6 {
            return sample_y(t);
        }
        let d = sample_dx(t);
        if d.abs() < 1e-6 {
            break;
        }
        t -= x2 / d;
    }
    let (mut lo, mut hi, mut t) = (0.0, 1.0, x);
    while lo < hi {
        let x2 = sample_x(t);
        if (x2 - x).abs() < 1e-6 {
            return sample_y(t);
        }
        if x > x2 {
            lo = t;
        } else {
            hi = t;
        }
        t = (hi - lo) * 0.5 + lo;
    }
    sample_y(t)
}
