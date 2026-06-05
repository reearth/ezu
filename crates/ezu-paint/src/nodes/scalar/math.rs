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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MathFn {
    // unary (a)
    Abs,
    Neg,
    Floor,
    Ceil,
    Round,
    Sqrt,
    // binary (a, b)
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Min,
    Max,
    Pow,
    // ternary (a, b, c)
    Clamp,
    Lerp,
}

impl MathFn {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "abs" => Self::Abs,
            "neg" => Self::Neg,
            "floor" => Self::Floor,
            "ceil" => Self::Ceil,
            "round" => Self::Round,
            "sqrt" => Self::Sqrt,
            "add" => Self::Add,
            "sub" => Self::Sub,
            "mul" => Self::Mul,
            "div" => Self::Div,
            "mod" => Self::Mod,
            "min" => Self::Min,
            "max" => Self::Max,
            "pow" => Self::Pow,
            "clamp" => Self::Clamp,
            "lerp" => Self::Lerp,
            _ => return None,
        })
    }

    /// Number of operands (1 = `a`, 2 = `a b`, 3 = `a b c`).
    fn arity(self) -> usize {
        match self {
            Self::Abs | Self::Neg | Self::Floor | Self::Ceil | Self::Round | Self::Sqrt => 1,
            Self::Add
            | Self::Sub
            | Self::Mul
            | Self::Div
            | Self::Mod
            | Self::Min
            | Self::Max
            | Self::Pow => 2,
            Self::Clamp | Self::Lerp => 3,
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Self::Abs => "abs",
            Self::Neg => "neg",
            Self::Floor => "floor",
            Self::Ceil => "ceil",
            Self::Round => "round",
            Self::Sqrt => "sqrt",
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mul => "mul",
            Self::Div => "div",
            Self::Mod => "mod",
            Self::Min => "min",
            Self::Max => "max",
            Self::Pow => "pow",
            Self::Clamp => "clamp",
            Self::Lerp => "lerp",
        }
    }

    fn apply(self, a: f64, b: f64, c: f64) -> f64 {
        match self {
            Self::Abs => a.abs(),
            Self::Neg => -a,
            Self::Floor => a.floor(),
            Self::Ceil => a.ceil(),
            Self::Round => a.round(),
            Self::Sqrt => a.sqrt(),
            Self::Add => a + b,
            Self::Sub => a - b,
            Self::Mul => a * b,
            Self::Div => a / b,
            Self::Mod => a.rem_euclid(b),
            Self::Min => a.min(b),
            Self::Max => a.max(b),
            Self::Pow => a.powf(b),
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
        let fn_name = read_string_or(fields, "fn", ctx, "")?;
        let func = MathFn::parse(&fn_name).ok_or_else(|| FactoryError::BadField {
            field: "fn".into(),
            msg: format!(
                "unknown fn `{fn_name}` (expected abs/neg/floor/ceil/round/sqrt/add/sub/mul/div/mod/min/max/pow/clamp/lerp)"
            ),
        })?;

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
            "description": "Arithmetic over scalar numbers. Operands accept literals, `$param` references, and `@node` scalar ports. Unary fns use `a`; binary `a b`; `clamp(a, b=lo, c=hi)`, `lerp(a, b, c=t)`.",
            "properties": {
                "fn": { "type": "string", "enum": [
                    "abs", "neg", "floor", "ceil", "round", "sqrt",
                    "add", "sub", "mul", "div", "mod", "min", "max", "pow",
                    "clamp", "lerp"
                ] },
                "a": ezu_graph::schema_frag::number(),
                "b": ezu_graph::schema_frag::number(),
                "c": ezu_graph::schema_frag::number(),
            },
            "required": ["fn", "a"],
        })
    }
}

ezu_graph::submit_node!(MathFactory);
