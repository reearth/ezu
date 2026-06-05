//! `threshold` — `ScalarField -> ScalarField`. Binarise scalar values
//! against `value`: outputs `low` (default `0.0`) for samples ≤
//! `value`, `high` (default `1.0`) otherwise. Optional `softness`
//! gives a linear ramp around the threshold instead of a hard step.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

struct ThresholdNode {
    value: In<f64>,
    softness: In<f64>,
    low: In<f64>,
    high: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for ThresholdNode {
    fn op_name(&self) -> &'static str {
        "threshold"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::ScalarField
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let field = inputs[0]
            .as_ref()
            .and_then(PortValue::as_scalar_field)
            .ok_or_else(|| EvalError::MissingInput("field".into()))?;
        let value = self.value.get(ctx, inputs)? as f32;
        let softness = self.softness.get(ctx, inputs)?.max(0.0) as f32;
        let low = self.low.get(ctx, inputs)? as f32;
        let high = self.high.get(ctx, inputs)? as f32;
        let half = softness * 0.5;
        let lo = value - half;
        let hi = value + half;
        let mut out: Vec<f32> = Vec::with_capacity(field.values.len());
        for &v in field.values.iter() {
            let t = if softness <= 0.0 {
                if v <= value {
                    0.0
                } else {
                    1.0
                }
            } else if v <= lo {
                0.0
            } else if v >= hi {
                1.0
            } else {
                (v - lo) / softness
            };
            out.push(low + t * (high - low));
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
        self.value.param_hash(h);
        self.softness.param_hash(h);
        self.low.param_hash(h);
        self.high.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        let mut r = InReader::new(fields, ctx, 1);
        let value = r.number_or("value", 0.5)?;
        let softness = r.number_or("softness", 0.0)?;
        let low = r.number_or("low", 0.0)?;
        let high = r.number_or("high", 1.0)?;
        let parts = r.finish();

        let mut ports = vec![PortSpec {
            name: "field",
            accepts: &[PortKind::ScalarField],
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "field".into(),
            src: input,
        }];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(ThresholdNode {
                value,
                softness,
                low,
                high,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Binarise a scalar field: emit `low` for samples ≤ `value`, `high` otherwise. With `softness > 0` the transition is a linear ramp of width `softness` centred on `value`.",
            "properties": {
                "field": schema_frag::node_ref(),
                "value": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.5 })),
                "softness": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "default": 0.0 })),
                "low": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.0 })),
                "high": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 1.0 })),
            },
            "required": ["field"],
        })
    }
}

ezu_graph::submit_node!(ThresholdFactory);
