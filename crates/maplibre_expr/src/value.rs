//! Runtime values produced by evaluating an expression.

use std::collections::BTreeMap;
use std::fmt;

use crate::color::Color;

/// A value in the MapLibre expression type system.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Color(Color),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

impl Value {
    /// The MapLibre type name of this value (`"number"`, `"string"`, ...).
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Color(_) => "color",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// Truthiness per the MapLibre `to-boolean` rules.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0.0 && !n.is_nan(),
            Value::String(s) => !s.is_empty(),
            _ => true,
        }
    }

    /// Build a literal [`Value`] from raw JSON (used by the `literal` operator
    /// and by bare literals in an expression).
    pub fn from_json(json: &serde_json::Value) -> Value {
        match json {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => Value::Number(n.as_f64().unwrap_or(f64::NAN)),
            serde_json::Value::String(s) => Value::String(s.clone()),
            serde_json::Value::Array(a) => Value::Array(a.iter().map(Value::from_json).collect()),
            serde_json::Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, v)| (k.clone(), Value::from_json(v)))
                    .collect(),
            ),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, ""),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Number(n) => write!(f, "{}", format_number(*n)),
            Value::String(s) => write!(f, "{s}"),
            Value::Color(c) => write!(f, "{c}"),
            Value::Array(a) => {
                let parts: Vec<String> = a.iter().map(|v| v.to_string()).collect();
                write!(f, "{}", parts.join(","))
            }
            Value::Object(_) => write!(f, "{self:?}"),
        }
    }
}

/// Format a number the way JavaScript's `String(n)` would (no trailing `.0`).
pub fn format_number(n: f64) -> String {
    if n == n.trunc() && n.is_finite() && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}
