//! `map-range` — `ScalarField -> ScalarField`. Linearly remap values
//! from `[in_min, in_max]` to `[out_min, out_max]`. Optionally clamps
//! results into the output range. Useful for normalising a DEM or
//! distance field into `[0, 1]` before feeding `color-ramp`, or for
//! amplifying / inverting a scalar signal before another scalar op.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, GeoScale, Node, NodeFactory, PortKind, PortSpec, PortValue, ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::read_number_or;

struct MapRangeNode {
    in_min: f32,
    in_max: f32,
    out_min: f32,
    out_max: f32,
    clamp: bool,
}

impl Node for MapRangeNode {
    fn op_name(&self) -> &'static str {
        "map-range"
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
        let span = self.in_max - self.in_min;
        // Degenerate input range: emit the output midpoint everywhere.
        // Avoids NaN from div-by-zero without forcing the caller to
        // special-case it.
        let inv_span = if span.abs() < 1e-9 { 0.0 } else { 1.0 / span };
        let mid = 0.5 * (self.out_min + self.out_max);
        let mut out: Vec<f32> = Vec::with_capacity(field.values.len());
        for &v in field.values.iter() {
            let t = (v - self.in_min) * inv_span;
            let mut y = self.out_min + t * (self.out_max - self.out_min);
            if inv_span == 0.0 {
                y = mid;
            }
            if self.clamp {
                let (lo, hi) = if self.out_min <= self.out_max {
                    (self.out_min, self.out_max)
                } else {
                    (self.out_max, self.out_min)
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
        h.update(&self.in_min.to_le_bytes());
        h.update(&self.in_max.to_le_bytes());
        h.update(&self.out_min.to_le_bytes());
        h.update(&self.out_max.to_le_bytes());
        h.update(&[self.clamp as u8]);
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
        let in_min = read_number_or(fields, "in-min", ctx, 0.0)? as f32;
        let in_max = read_number_or(fields, "in-max", ctx, 1.0)? as f32;
        let out_min = read_number_or(fields, "out-min", ctx, 0.0)? as f32;
        let out_max = read_number_or(fields, "out-max", ctx, 1.0)? as f32;
        let clamp = fields
            .get("clamp")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(BuiltNode {
            node: Box::new(MapRangeNode {
                in_min,
                in_max,
                out_min,
                out_max,
                clamp,
            }),
            connections: vec![Connection {
                port: "field".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Linearly remap scalar field values from [in-min, in-max] to [out-min, out-max]. With `clamp: true`, results outside the output range are pinned to the range bounds. Useful for normalising elevation or distance fields before a `color-ramp`.",
            "properties": {
                "field": schema_frag::node_ref(),
                "in-min": { "type": "number", "default": 0.0 },
                "in-max": { "type": "number", "default": 1.0 },
                "out-min": { "type": "number", "default": 0.0 },
                "out-max": { "type": "number", "default": 1.0 },
                "clamp": { "type": "boolean", "default": false },
            },
            "required": ["field"],
        })
    }
}

ezu_graph::submit_node!(MapRangeFactory);
