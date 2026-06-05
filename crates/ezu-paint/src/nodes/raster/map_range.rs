//! `map-range` — `ScalarField -> ScalarField`. Linearly remap values
//! from `[in_min, in_max]` to `[out_min, out_max]`. Optionally clamps
//! results into the output range. Useful for normalising a DEM or
//! distance field into `[0, 1]` before feeding `color-ramp`, or for
//! amplifying / inverting a scalar signal before another scalar op.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, GeoScale, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
    ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

struct MapRangeNode {
    in_min: In<f64>,
    in_max: In<f64>,
    out_min: In<f64>,
    out_max: In<f64>,
    clamp: In<bool>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for MapRangeNode {
    fn op_name(&self) -> &'static str {
        "map-range"
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
        let in_min = self.in_min.get(ctx, inputs)? as f32;
        let in_max = self.in_max.get(ctx, inputs)? as f32;
        let out_min = self.out_min.get(ctx, inputs)? as f32;
        let out_max = self.out_max.get(ctx, inputs)? as f32;
        let clamp = self.clamp.get(ctx, inputs)?;
        let span = in_max - in_min;
        // Degenerate input range: emit the output midpoint everywhere.
        // Avoids NaN from div-by-zero without forcing the caller to
        // special-case it.
        let inv_span = if span.abs() < 1e-9 { 0.0 } else { 1.0 / span };
        let mid = 0.5 * (out_min + out_max);
        let mut out: Vec<f32> = Vec::with_capacity(field.values.len());
        for &v in field.values.iter() {
            let t = (v - in_min) * inv_span;
            let mut y = out_min + t * (out_max - out_min);
            if inv_span == 0.0 {
                y = mid;
            }
            if clamp {
                let (lo, hi) = if out_min <= out_max {
                    (out_min, out_max)
                } else {
                    (out_max, out_min)
                };
                y = y.clamp(lo, hi);
            }
            out.push(y);
        }
        Ok(PortValue::ScalarField(Arc::new(ScalarField {
            width: field.width,
            height: field.height,
            values: out.into(),
            nodata: field.nodata,
            // Remapping values doesn't change geographic spacing —
            // a remapped DEM still has the same metres-per-pixel.
            geo_scale: field.geo_scale.map(|g| GeoScale {
                metres_per_pixel_x: g.metres_per_pixel_x,
                metres_per_pixel_y: g.metres_per_pixel_y,
            }),
        })))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"map-range");
        self.in_min.param_hash(h);
        self.in_max.param_hash(h);
        self.out_min.param_hash(h);
        self.out_max.param_hash(h);
        self.clamp.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct MapRangeFactory;
impl NodeFactory for MapRangeFactory {
    fn op_name(&self) -> &'static str {
        "map-range"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "field")?;
        let mut r = InReader::new(fields, ctx, 1);
        let in_min = r.number_or("in-min", 0.0)?;
        let in_max = r.number_or("in-max", 1.0)?;
        let out_min = r.number_or("out-min", 0.0)?;
        let out_max = r.number_or("out-max", 1.0)?;
        let clamp = r.bool_or("clamp", false)?;
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
            node: Box::new(MapRangeNode {
                in_min,
                in_max,
                out_min,
                out_max,
                clamp,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Linearly remap scalar field values from [in-min, in-max] to [out-min, out-max]. With `clamp: true`, results outside the output range are pinned to the range bounds. Useful for normalising elevation or distance fields before a `color-ramp`.",
            "properties": {
                "field": schema_frag::node_ref(),
                "in-min": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.0 })),
                "in-max": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 1.0 })),
                "out-min": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.0 })),
                "out-max": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 1.0 })),
                "clamp": { "oneOf": [{"type": "boolean"}, {"type": "string", "pattern": "^[$@].+"}], "default": false },
            },
            "required": ["field"],
        })
    }
}

ezu_graph::submit_node!(MapRangeFactory);
