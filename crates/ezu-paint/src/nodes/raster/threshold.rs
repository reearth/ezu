//! `threshold` — `ScalarField -> ScalarField`. Binarise scalar values
//! against `value`: outputs `low` (default `0.0`) for samples ≤
//! `value`, `high` (default `1.0`) otherwise. Optional `softness`
//! gives a linear ramp around the threshold instead of a hard step.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::read_number_or;

struct ThresholdNode {
    value: f32,
    softness: f32,
    low: f32,
    high: f32,
}

impl Node for ThresholdNode {
    fn op_name(&self) -> &'static str {
        "threshold"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "field",
            accepts: &[PortKind::ScalarField],
            optional: false,
        }];
        SPECS
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::ScalarField
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let field = inputs[0]
            .as_ref()
            .and_then(PortValue::as_scalar_field)
            .ok_or_else(|| EvalError::MissingInput("field".into()))?;
        let half = self.softness * 0.5;
        let lo = self.value - half;
        let hi = self.value + half;
        let mut out: Vec<f32> = Vec::with_capacity(field.values.len());
        for &v in field.values.iter() {
            let t = if self.softness <= 0.0 {
                if v <= self.value {
                    0.0
                } else {
                    1.0
                }
            } else if v <= lo {
                0.0
            } else if v >= hi {
                1.0
            } else {
                (v - lo) / self.softness
            };
            out.push(self.low + t * (self.high - self.low));
        }
        Ok(PortValue::ScalarField(Arc::new(ScalarField {
            width: field.width,
            height: field.height,
            values: out.into(),
            nodata: field.nodata,
            geo_scale: field.geo_scale,
        })))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"threshold");
        h.update(&self.value.to_le_bytes());
        h.update(&self.softness.to_le_bytes());
        h.update(&self.low.to_le_bytes());
        h.update(&self.high.to_le_bytes());
    }
}

pub(super) struct ThresholdFactory;
impl NodeFactory for ThresholdFactory {
    fn op_name(&self) -> &'static str {
        "threshold"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "field")?;
        let value = read_number_or(fields, "value", ctx, 0.5)? as f32;
        let softness = read_number_or(fields, "softness", ctx, 0.0)?.max(0.0) as f32;
        let low = read_number_or(fields, "low", ctx, 0.0)? as f32;
        let high = read_number_or(fields, "high", ctx, 1.0)? as f32;
        Ok(BuiltNode {
            node: Box::new(ThresholdNode {
                value,
                softness,
                low,
                high,
            }),
            connections: vec![Connection {
                port: "field".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Binarise a scalar field: emit `low` for samples ≤ `value`, `high` otherwise. With `softness > 0` the transition is a linear ramp of width `softness` centred on `value`.",
            "properties": {
                "field": schema_frag::node_ref(),
                "value": { "type": "number", "default": 0.5 },
                "softness": { "type": "number", "minimum": 0.0, "default": 0.0 },
                "low": { "type": "number", "default": 0.0 },
                "high": { "type": "number", "default": 1.0 },
            },
            "required": ["field"],
        })
    }
}

ezu_graph::submit_node!(ThresholdFactory);
