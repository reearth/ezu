//! `math` — arithmetic over scalar numbers. Operands are `In<f64>`
//! fields: literals, `$param` references, or `@node` scalar ports, so
//! chains like `zoom → math → blur.sigma` and `$k * 2` both work.
//!
//! ```json
//! { "op": "math", "fn": "mul", "a": "$k", "b": 2 }
//! { "op": "math", "fn": "lerp", "a": 4, "b": 12, "c": "@t" }
//! ```

use ezu_graph::{
    BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, In, InReader, Node, NodeFactory,
    PortKind, PortSpec, PortValue, ScalarValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::read_string_or;

/// One of `math`'s functions. `field-math` applies the same set per
/// pixel, so its arity and semantics live here once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::nodes) enum MathFn {
    // unary (a)
    Abs,
    Neg,
    Floor,
    Ceil,
    Round,
    Fract,
    Sign,
    Sqrt,
    Exp,
    Log,
    Log2,
    Log10,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    // binary (a, b)
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Min,
    Max,
    Pow,
    Atan2,
    Step,
    // ternary (a, b, c)
    Clamp,
    Lerp,
    Smoothstep,
}

impl MathFn {
    /// Every function name, in the order the schema lists them.
    pub(in crate::nodes) const NAMES: [&'static str; 31] = [
        "abs",
        "neg",
        "floor",
        "ceil",
        "round",
        "fract",
        "sign",
        "sqrt",
        "exp",
        "log",
        "log2",
        "log10",
        "sin",
        "cos",
        "tan",
        "asin",
        "acos",
        "atan",
        "add",
        "sub",
        "mul",
        "div",
        "mod",
        "min",
        "max",
        "pow",
        "atan2",
        "step",
        "clamp",
        "lerp",
        "smoothstep",
    ];

    /// How the schema describes the operands, shared by `math` and
    /// `field-math`.
    pub(in crate::nodes) const OPERANDS_DOC: &'static str = "Unary fns use `a`; binary `a b`; `clamp(a, b=lo, c=hi)`, `lerp(a, b, c=t)`, `smoothstep(a=edge0, b=edge1, c=x)`. `log` is the natural logarithm; angles are in radians; `atan2(a=y, b=x)`; `step(a=edge, b=x)` is 0 below the edge and 1 from it on; `fract` is `a - floor(a)`; `sign` is -1, 0 or 1.";

    /// Read the node's `fn` field.
    pub(in crate::nodes) fn read(
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<Self, FactoryError> {
        let fn_name = read_string_or(fields, "fn", ctx, "")?;
        Self::parse(&fn_name).ok_or_else(|| FactoryError::BadField {
            field: "fn".into(),
            msg: format!(
                "unknown fn `{fn_name}` (expected {})",
                Self::NAMES.join("/")
            ),
        })
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "abs" => Self::Abs,
            "neg" => Self::Neg,
            "floor" => Self::Floor,
            "ceil" => Self::Ceil,
            "round" => Self::Round,
            "fract" => Self::Fract,
            "sign" => Self::Sign,
            "sqrt" => Self::Sqrt,
            "exp" => Self::Exp,
            "log" => Self::Log,
            "log2" => Self::Log2,
            "log10" => Self::Log10,
            "sin" => Self::Sin,
            "cos" => Self::Cos,
            "tan" => Self::Tan,
            "asin" => Self::Asin,
            "acos" => Self::Acos,
            "atan" => Self::Atan,
            "add" => Self::Add,
            "sub" => Self::Sub,
            "mul" => Self::Mul,
            "div" => Self::Div,
            "mod" => Self::Mod,
            "min" => Self::Min,
            "max" => Self::Max,
            "pow" => Self::Pow,
            "atan2" => Self::Atan2,
            "step" => Self::Step,
            "clamp" => Self::Clamp,
            "lerp" => Self::Lerp,
            "smoothstep" => Self::Smoothstep,
            _ => return None,
        })
    }

    /// Number of operands (1 = `a`, 2 = `a b`, 3 = `a b c`).
    pub(in crate::nodes) fn arity(self) -> usize {
        match self {
            Self::Abs
            | Self::Neg
            | Self::Floor
            | Self::Ceil
            | Self::Round
            | Self::Fract
            | Self::Sign
            | Self::Sqrt
            | Self::Exp
            | Self::Log
            | Self::Log2
            | Self::Log10
            | Self::Sin
            | Self::Cos
            | Self::Tan
            | Self::Asin
            | Self::Acos
            | Self::Atan => 1,
            Self::Add
            | Self::Sub
            | Self::Mul
            | Self::Div
            | Self::Mod
            | Self::Min
            | Self::Max
            | Self::Pow
            | Self::Atan2
            | Self::Step => 2,
            Self::Clamp | Self::Lerp | Self::Smoothstep => 3,
        }
    }

    pub(in crate::nodes) fn tag(self) -> &'static str {
        match self {
            Self::Abs => "abs",
            Self::Neg => "neg",
            Self::Floor => "floor",
            Self::Ceil => "ceil",
            Self::Round => "round",
            Self::Fract => "fract",
            Self::Sign => "sign",
            Self::Sqrt => "sqrt",
            Self::Exp => "exp",
            Self::Log => "log",
            Self::Log2 => "log2",
            Self::Log10 => "log10",
            Self::Sin => "sin",
            Self::Cos => "cos",
            Self::Tan => "tan",
            Self::Asin => "asin",
            Self::Acos => "acos",
            Self::Atan => "atan",
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mul => "mul",
            Self::Div => "div",
            Self::Mod => "mod",
            Self::Min => "min",
            Self::Max => "max",
            Self::Pow => "pow",
            Self::Atan2 => "atan2",
            Self::Step => "step",
            Self::Clamp => "clamp",
            Self::Lerp => "lerp",
            Self::Smoothstep => "smoothstep",
        }
    }

    /// Operands past the function's arity are ignored. A result that is
    /// not finite (`div` or `mod` by zero, `sqrt` or `log` of a negative,
    /// …) is returned as is; each caller decides what that means for it.
    /// The transcendental functions go through `libm` so every host
    /// rounds them the same way.
    pub(in crate::nodes) fn apply(self, a: f64, b: f64, c: f64) -> f64 {
        match self {
            Self::Abs => a.abs(),
            Self::Neg => -a,
            Self::Floor => a.floor(),
            Self::Ceil => a.ceil(),
            Self::Round => a.round(),
            Self::Fract => a - a.floor(),
            // `signum` calls zero positive; a sign function should not.
            Self::Sign => {
                if a > 0.0 {
                    1.0
                } else if a < 0.0 {
                    -1.0
                } else {
                    a
                }
            }
            Self::Sqrt => a.sqrt(),
            Self::Exp => libm::exp(a),
            Self::Log => libm::log(a),
            Self::Log2 => libm::log2(a),
            Self::Log10 => libm::log10(a),
            Self::Sin => libm::sin(a),
            Self::Cos => libm::cos(a),
            Self::Tan => libm::tan(a),
            Self::Asin => libm::asin(a),
            Self::Acos => libm::acos(a),
            Self::Atan => libm::atan(a),
            Self::Atan2 => libm::atan2(a, b),
            Self::Step => {
                if b < a {
                    0.0
                } else {
                    1.0
                }
            }
            Self::Smoothstep => {
                // Equal edges make a hard step rather than a division by
                // zero.
                if a == b {
                    return if c < a { 0.0 } else { 1.0 };
                }
                let t = ((c - a) / (b - a)).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            }
            Self::Add => a + b,
            Self::Sub => a - b,
            Self::Mul => a * b,
            Self::Div => a / b,
            Self::Mod => a.rem_euclid(b),
            Self::Min => a.min(b),
            Self::Max => a.max(b),
            Self::Pow => libm::pow(a, b),
            Self::Clamp => a.clamp(b.min(c), c.max(b)),
            Self::Lerp => a + (b - a) * c,
        }
    }
}

struct MathNode {
    func: MathFn,
    a: In<f64>,
    b: Option<In<f64>>,
    c: Option<In<f64>>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for MathNode {
    fn op_name(&self) -> &'static str {
        "math"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Scalar
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let a = self.a.get(ctx, inputs)?;
        let b = match &self.b {
            Some(v) => v.get(ctx, inputs)?,
            None => 0.0,
        };
        let c = match &self.c {
            Some(v) => v.get(ctx, inputs)?,
            None => 0.0,
        };
        let out = self.func.apply(a, b, c);
        if !out.is_finite() {
            return Err(EvalError::Other(format!(
                "math `{}`: non-finite result ({a} , {b} , {c})",
                self.func.tag()
            )));
        }
        Ok(PortValue::Scalar(ScalarValue::Number(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"math");
        h.update(self.func.tag().as_bytes());
        self.a.param_hash(h);
        if let Some(b) = &self.b {
            b.param_hash(h);
        }
        if let Some(c) = &self.c {
            c.param_hash(h);
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct MathFactory;
impl NodeFactory for MathFactory {
    fn op_name(&self) -> &'static str {
        "math"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let func = MathFn::read(fields, ctx)?;

        let mut r = InReader::new(fields, ctx, 0);
        let a = r.number("a")?;
        let b = if func.arity() >= 2 {
            Some(r.number("b")?)
        } else {
            None
        };
        let c = if func.arity() >= 3 {
            Some(r.number("c")?)
        } else {
            None
        };
        let parts = r.finish();

        Ok(BuiltNode {
            node: Box::new(MathNode {
                func,
                a,
                b,
                c,
                ports: parts.ports,
                param_refs: parts.param_refs,
            }),
            connections: parts.connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": format!(
                "Arithmetic over scalar numbers. Operands accept literals, `$param` references, and `@node` scalar ports. {}",
                MathFn::OPERANDS_DOC
            ),
            "properties": {
                "fn": { "type": "string", "enum": MathFn::NAMES },
                "a": ezu_graph::schema_frag::number(),
                "b": ezu_graph::schema_frag::number(),
                "c": ezu_graph::schema_frag::number(),
            },
            "required": ["fn", "a"],
        })
    }
}

ezu_graph::submit_node!(MathFactory);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_parses_to_a_function_with_that_tag() {
        for name in MathFn::NAMES {
            let f = MathFn::parse(name).unwrap_or_else(|| panic!("`{name}` does not parse"));
            assert_eq!(f.tag(), name);
        }
    }

    #[test]
    fn exponentials_and_logarithms() {
        let e = std::f64::consts::E;
        assert_eq!(MathFn::Exp.apply(0.0, 0.0, 0.0), 1.0);
        assert!((MathFn::Exp.apply(1.0, 0.0, 0.0) - e).abs() < 1e-15);
        assert!((MathFn::Log.apply(e, 0.0, 0.0) - 1.0).abs() < 1e-15);
        assert_eq!(MathFn::Log2.apply(8.0, 0.0, 0.0), 3.0);
        assert_eq!(MathFn::Log10.apply(1000.0, 0.0, 0.0), 3.0);
        assert!(MathFn::Log.apply(-1.0, 0.0, 0.0).is_nan());
        assert_eq!(MathFn::Log.apply(0.0, 0.0, 0.0), f64::NEG_INFINITY);
    }

    #[test]
    fn trigonometry_is_in_radians() {
        use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};
        let close = |a: f64, b: f64| (a - b).abs() < 1e-12;
        assert!(close(MathFn::Sin.apply(FRAC_PI_2, 0.0, 0.0), 1.0));
        assert!(close(MathFn::Cos.apply(PI, 0.0, 0.0), -1.0));
        assert!(close(MathFn::Tan.apply(FRAC_PI_4, 0.0, 0.0), 1.0));
        assert!(close(MathFn::Asin.apply(1.0, 0.0, 0.0), FRAC_PI_2));
        assert!(close(MathFn::Acos.apply(-1.0, 0.0, 0.0), PI));
        assert!(close(MathFn::Atan.apply(1.0, 0.0, 0.0), FRAC_PI_4));
        // atan2(y, x): the quadrant comes from both signs.
        assert!(close(MathFn::Atan2.apply(1.0, -1.0, 0.0), 3.0 * FRAC_PI_4));
        assert!(close(MathFn::Atan2.apply(-1.0, 0.0, 0.0), -FRAC_PI_2));
    }

    #[test]
    fn sign_fract_and_step() {
        assert_eq!(MathFn::Sign.apply(-3.5, 0.0, 0.0), -1.0);
        assert_eq!(MathFn::Sign.apply(0.0, 0.0, 0.0), 0.0);
        assert_eq!(MathFn::Sign.apply(2.0, 0.0, 0.0), 1.0);
        assert_eq!(MathFn::Fract.apply(2.75, 0.0, 0.0), 0.75);
        // Like `mod`, it counts up from the floor below a negative.
        assert_eq!(MathFn::Fract.apply(-0.25, 0.0, 0.0), 0.75);
        assert_eq!(MathFn::Step.apply(1.0, 0.5, 0.0), 0.0);
        assert_eq!(MathFn::Step.apply(1.0, 1.0, 0.0), 1.0);
        assert_eq!(MathFn::Step.apply(1.0, 3.0, 0.0), 1.0);
    }

    #[test]
    fn smoothstep_eases_between_its_edges() {
        let s = |x: f64| MathFn::Smoothstep.apply(2.0, 4.0, x);
        assert_eq!(s(1.0), 0.0);
        assert_eq!(s(2.0), 0.0);
        assert_eq!(s(3.0), 0.5);
        assert_eq!(s(2.5), 0.15625);
        assert_eq!(s(4.0), 1.0);
        assert_eq!(s(9.0), 1.0);
        // Reversed edges fall instead of rise.
        assert_eq!(MathFn::Smoothstep.apply(4.0, 2.0, 2.5), 1.0 - 0.15625);
        // Equal edges are a hard step, not a division by zero.
        assert_eq!(MathFn::Smoothstep.apply(2.0, 2.0, 1.9), 0.0);
        assert_eq!(MathFn::Smoothstep.apply(2.0, 2.0, 2.0), 1.0);
    }
}
