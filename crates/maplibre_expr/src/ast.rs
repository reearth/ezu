//! The parsed expression tree.

use crate::value::Value;

/// A parsed MapLibre expression.
///
/// Most operators are represented uniformly as [`Expr::Call`]; the handful of
/// operators with irregular argument shapes (bindings, unquoted match labels,
/// stop lists) get dedicated variants so evaluation stays simple.
#[derive(Debug, Clone)]
pub enum Expr {
    /// A constant value (bare literal or the `literal` operator).
    Literal(Value),
    /// A generic operator call whose arguments are all sub-expressions.
    Call { op: String, args: Vec<Expr> },
    /// `["let", name, value, ..., body]`
    Let {
        bindings: Vec<(String, Expr)>,
        body: Box<Expr>,
    },
    /// `["var", name]`
    Var(String),
    /// `["match", input, label, output, ..., default]`
    Match {
        input: Box<Expr>,
        arms: Vec<(Vec<Value>, Expr)>,
        default: Box<Expr>,
    },
    /// `["step", input, output0, stop1, output1, ...]`
    Step {
        input: Box<Expr>,
        output0: Box<Expr>,
        stops: Vec<(f64, Expr)>,
    },
    /// `["interpolate"|"interpolate-hcl"|"interpolate-lab", type, input, stop, output, ...]`
    Interpolate {
        kind: InterpKind,
        space: InterpSpace,
        input: Box<Expr>,
        stops: Vec<(f64, Expr)>,
    },
}

/// The interpolation curve used by an `interpolate` expression.
#[derive(Debug, Clone, Copy)]
pub enum InterpKind {
    Linear,
    Exponential(f64),
    CubicBezier(f64, f64, f64, f64),
}

/// The color space an `interpolate` expression blends in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InterpSpace {
    /// Plain component-wise interpolation (`interpolate`).
    Rgb,
    /// `interpolate-hcl`
    Hcl,
    /// `interpolate-lab`
    Lab,
}
