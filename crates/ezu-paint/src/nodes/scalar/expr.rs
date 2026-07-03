//! `expr` — a MapLibre expression as a `Scalar`. Evaluates a raw
//! expression once per tile with the tile's zoom in the context, so any
//! zoom curve (`interpolate`, `step`, legacy `{stops}`) can drive an
//! `In<T>` field on any node through an `@node` reference:
//!
//! ```json
//! "fade": { "op": "expr", "expr": ["interpolate", ["linear"], ["zoom"], 13, 1, 15, 0] },
//! "ramp": { "op": "color-ramp", "field": "@dens", "ramp-expr": [...], "opacity": "@fade" }
//! ```
//!
//! There is no feature in the context — per-feature expressions belong on
//! the consuming node's `*-expr` fields, which evaluate per feature group.

use ezu_graph::{
    BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory, PortKind, PortSpec,
    PortValue, ScalarValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

/// Which scalar the expression must produce.
#[derive(Clone, Copy, PartialEq)]
enum ExprType {
    Number,
    Color,
    Bool,
}

struct ExprNode {
    expr: maplibre_expr::Expr,
    /// Raw `expr` JSON text, for a stable cache hash.
    expr_src: String,
    ty: ExprType,
}

impl Node for ExprNode {
    fn op_name(&self) -> &'static str {
        "expr"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Scalar
    }
    fn eval(&self, ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        let ectx = maplibre_expr::EvaluationContext::new().with_zoom(ctx.tile.z as f64);
        let v = maplibre_expr::evaluate(&self.expr, &ectx)
            .map_err(|e| EvalError::Other(format!("expr: evaluation failed: {e}")))?;
        let sv = match (self.ty, v) {
            (ExprType::Number, maplibre_expr::Value::Number(n)) => ScalarValue::Number(n),
            (ExprType::Color, maplibre_expr::Value::Color(c)) => {
                ScalarValue::Color([c.r as f32, c.g as f32, c.b as f32, c.a as f32])
            }
            (ExprType::Bool, maplibre_expr::Value::Bool(b)) => ScalarValue::Bool(b),
            (_, v) => {
                return Err(EvalError::Other(format!(
                    "expr: expected {}, got {v:?}",
                    type_name(self.ty)
                )))
            }
        };
        Ok(PortValue::Scalar(sv))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"expr");
        h.update(&[self.ty as u8]);
        h.update(self.expr_src.as_bytes());
    }
}

fn type_name(ty: ExprType) -> &'static str {
    match ty {
        ExprType::Number => "number",
        ExprType::Color => "color",
        ExprType::Bool => "bool",
    }
}

pub(super) struct ExprFactory;
impl NodeFactory for ExprFactory {
    fn op_name(&self) -> &'static str {
        "expr"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let raw = fields
            .get("expr")
            .ok_or_else(|| FactoryError::MissingField("expr".into()))?;
        let ty = match fields.get("type").and_then(Value::as_str) {
            None | Some("number") => ExprType::Number,
            Some("color") => ExprType::Color,
            Some("bool") => ExprType::Bool,
            Some(other) => {
                return Err(FactoryError::BadField {
                    field: "type".into(),
                    msg: format!("expected number/color/bool, got `{other}`"),
                })
            }
        };
        let expected = match ty {
            ExprType::Number => maplibre_expr::Type::Number,
            ExprType::Color => maplibre_expr::Type::Color,
            ExprType::Bool => maplibre_expr::Type::Boolean,
        };
        let expr = maplibre_expr::parse(raw).map_err(|e| FactoryError::BadField {
            field: "expr".into(),
            msg: e.to_string(),
        })?;
        let expr = maplibre_expr::typecheck(&expr, Some(&expected), false).map_err(|e| {
            FactoryError::BadField {
                field: "expr".into(),
                msg: e.to_string(),
            }
        })?;
        Ok(BuiltNode {
            node: Box::new(ExprNode {
                expr,
                expr_src: raw.to_string(),
                ty,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Evaluate a MapLibre expression once per tile (the tile's zoom is in the context) and emit the result as a Scalar — feed it to any node's scalar field via `@node`. Use for zoom curves (`interpolate`/`step`/legacy `{stops}`); per-feature expressions belong on the consuming node's `*-expr` fields instead.",
            "properties": {
                "expr": { "description": "A raw MapLibre expression producing `type` (e.g. [\"interpolate\", [\"linear\"], [\"zoom\"], 13, 1, 15, 0])." },
                "type": { "type": "string", "enum": ["number", "color", "bool"], "default": "number",
                          "description": "The scalar type the expression must produce (build-time typechecked)." },
            },
            "required": ["expr"],
        })
    }
}

ezu_graph::submit_node!(ExprFactory);
