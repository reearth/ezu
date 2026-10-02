//! `field-math` — arithmetic over scalar fields, one pixel at a time.
//! The same functions as `math`, with the same arities and semantics,
//! but at least one operand is a `ScalarField` and the result is one:
//! roughen a DEM with a little noise, mix two fields by a third, clamp
//! or rescale a field by a `$param`.
//!
//! Each of `a`, `b` and `c` is either a field — an `@node` that outputs
//! a `ScalarField` — or a number: a literal, a `$param`, or an `@node`
//! that outputs a `Scalar`. A number applies to every pixel alike.
//!
//! ```json
//! { "op": "field-math", "fn": "add", "a": "@dem", "b": "@bumps" }
//! { "op": "field-math", "fn": "mul", "a": "@noise", "b": "$roughness" }
//! { "op": "field-math", "fn": "lerp", "a": "@dem", "b": "@smooth", "c": "@mask" }
//! ```
//!
//! Every field operand must be the same size. A pixel where any field
//! operand is missing (NaN, or equal to that field's `nodata`) comes out
//! missing, as does one whose result is not a finite number — `sqrt` of
//! a negative, `div` or `mod` by zero, a `pow` that overflows. Where
//! `math` stops the render on such a result, a field just loses that
//! pixel. The output takes its `nodata` and `geo_scale` from the first
//! field operand in `a`, `b`, `c` order, and writes missing pixels as
//! that `nodata`, or NaN when it has none.
//!
//! Each pixel is computed in `f64`, exactly as `math` would compute it
//! from the same numbers, and rounded to `f32` once when stored.

use std::sync::Arc;

use ezu_graph::{
    BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, In, InReader, Node, NodeFactory,
    PortKind, PortSpec, PortValue, ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::field_fill;
use crate::nodes::scalar::math::MathFn;

/// What an `@node` operand may be wired to: a field, or a single number
/// that applies to every pixel.
const ACCEPTS_FIELD_OR_SCALAR: &[PortKind] = &[PortKind::ScalarField, PortKind::Scalar];

const NO_FIELD: &str = "`field-math` needs at least one scalar-field operand: wire `a`, `b` or \
                        `c` to a node that outputs a ScalarField (`dem`, `noise` with \
                        `kind: \"scalar\"`, …). For numbers alone, use `math`";

/// One operand, resolved for this eval.
#[derive(Clone, Copy)]
enum Operand<'a> {
    Field(&'a ScalarField),
    Number(f64),
}

impl Operand<'_> {
    /// The operand's value at pixel `i`, or `None` where it is missing.
    #[inline]
    fn at(self, i: usize) -> Option<f64> {
        match self {
            Operand::Field(f) => {
                let v = f.values[i];
                if v.is_nan() || f.nodata == Some(v) {
                    None
                } else {
                    Some(f64::from(v))
                }
            }
            Operand::Number(n) => Some(n),
        }
    }
}

/// Resolve an operand: a port carrying a field is that field, and
/// everything else — a literal, a `$param`, a port carrying a scalar —
/// is the number [`In::get`] reads.
fn resolve<'a>(
    operand: &In<f64>,
    ctx: &EvalCtx<'_>,
    inputs: &'a [Option<PortValue>],
) -> Result<Operand<'a>, EvalError> {
    if let In::Port { ix, .. } = operand {
        if let Some(Some(PortValue::ScalarField(f))) = inputs.get(*ix) {
            return Ok(Operand::Field(f));
        }
    }
    operand.get(ctx, inputs).map(Operand::Number)
}

struct FieldMathNode {
    func: MathFn,
    a: In<f64>,
    b: Option<In<f64>>,
    c: Option<In<f64>>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for FieldMathNode {
    fn op_name(&self) -> &'static str {
        "field-math"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn validate_kinds(&self, input_kinds: &[Option<PortKind>]) -> Result<(), String> {
        if input_kinds.contains(&Some(PortKind::ScalarField)) {
            Ok(())
        } else {
            Err(NO_FIELD.to_string())
        }
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::ScalarField
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let tag = self.func.tag();
        let mut operands = [("a", Operand::Number(0.0)); 3];
        operands[0].1 = resolve(&self.a, ctx, inputs)?;
        if let Some(b) = &self.b {
            operands[1] = ("b", resolve(b, ctx, inputs)?);
        }
        if let Some(c) = &self.c {
            operands[2] = ("c", resolve(c, ctx, inputs)?);
        }

        // The first field sets the size, `nodata` and `geo_scale`; every
        // other field has to match its size.
        let mut first: Option<(&str, &ScalarField)> = None;
        for &(name, op) in &operands {
            let Operand::Field(f) = op else { continue };
            match first {
                None => first = Some((name, f)),
                Some((first_name, ff)) if (f.width, f.height) != (ff.width, ff.height) => {
                    return Err(EvalError::Other(format!(
                        "field-math `{tag}`: `{name}` is {}×{} but `{first_name}` is {}×{}; \
                         every field operand must be the same size",
                        f.width, f.height, ff.width, ff.height
                    )));
                }
                Some(_) => {}
            }
        }
        let Some((_, first)) = first else {
            return Err(EvalError::Other(NO_FIELD.to_string()));
        };

        // Deterministic on every host: each pixel is one fixed sequence
        // of IEEE 754 operations in `f64` (`+ - * /`, `sqrt`, rounding,
        // `%`, all exactly rounded, and `libm::pow`), with no reduction
        // across pixels, and one rounding to `f32` at the end.
        let fill = field_fill(first);
        let [(_, a), (_, b), (_, c)] = operands;
        let values: Vec<f32> = (0..first.values.len())
            .map(|i| {
                let (Some(x), Some(y), Some(z)) = (a.at(i), b.at(i), c.at(i)) else {
                    return fill;
                };
                let v = self.func.apply(x, y, z) as f32;
                if v.is_finite() {
                    v
                } else {
                    fill
                }
            })
            .collect();
        Ok(PortValue::ScalarField(Arc::new(ScalarField {
            values: values.into(),
            ..*first
        })))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"field-math");
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

pub(super) struct FieldMathFactory;
impl NodeFactory for FieldMathFactory {
    fn op_name(&self) -> &'static str {
        "field-math"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let func = MathFn::read(fields, ctx)?;

        // Operands read as `math`'s do, so a literal, a `$param` and a
        // scalar port behave exactly as they would there. An `@node`
        // becomes a port, which here also takes a field: whether it is
        // one is known from the port's kind, checked in `validate_kinds`
        // and resolved in `eval`.
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
        let mut parts = r.finish();
        if parts.ports.is_empty() {
            return Err(FactoryError::Custom(NO_FIELD.to_string()));
        }
        for port in &mut parts.ports {
            port.accepts = ACCEPTS_FIELD_OR_SCALAR;
        }

        Ok(BuiltNode {
            node: Box::new(FieldMathNode {
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
        let operand = serde_json::json!({
            "oneOf": [
                { "type": "number" },
                {
                    "type": "string",
                    "pattern": "^[$@][A-Za-z_][A-Za-z0-9_-]*$",
                    "description": "`$param` reference, or `@node` that outputs a ScalarField or a Scalar."
                }
            ]
        });
        serde_json::json!({
            "description": "Per-pixel arithmetic over scalar fields, with the same functions as `math`: unary fns use `a`; binary `a b`; `clamp(a, b=lo, c=hi)`, `lerp(a, b, c=t)`. Each operand is a field (`@node` outputting a ScalarField) or a number (literal, `$param`, or `@node` scalar port) applied to every pixel; at least one must be a field, and all fields must be the same size. Add a little noise to a DEM to roughen it, mix two fields by a third with `lerp`, or scale a field by a `$param`. A pixel is missing in the output where any field operand is missing (NaN or its nodata value), or where the result is not a finite number (`sqrt` of a negative, `div` by zero). The output keeps the nodata value and geographic scale of the first field operand, and writes missing pixels as that nodata, or NaN.",
            "properties": {
                "fn": { "type": "string", "enum": MathFn::NAMES },
                "a": operand,
                "b": operand,
                "c": operand,
            },
            "required": ["fn", "a"],
        })
    }
}

ezu_graph::submit_node!(FieldMathFactory);
