//! `slope` — `ScalarField -> Raster`. Per-pixel slope angle from a
//! 3×3 Horn gradient, normalised to `0..1` against `max-deg` and
//! rasterised as grayscale RGBA.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::terrain_common::horn_gradient;

struct SlopeNode {
    max_deg: In<f64>,
    invert: In<bool>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for SlopeNode {
    fn op_name(&self) -> &'static str {
        "slope"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
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
        let max_deg = self.max_deg.get(ctx, inputs)? as f32;
        let invert = self.invert.get(ctx, inputs)?;
        let w = field.width;
        let h = field.height;
        let mut out = RasterBuf::new(w, h);
        let inv_x = 1.0 / (8.0 * field.metres_per_pixel_x().max(1e-6));
        let inv_y = 1.0 / (8.0 * field.metres_per_pixel_y().max(1e-6));
        let max_rad = max_deg.to_radians().max(1e-4);
        for y in 0..h {
            for x in 0..w {
                let (dz_dx, dz_dy) = horn_gradient(field, x, y, inv_x, inv_y);
                let slope = (dz_dx * dz_dx + dz_dy * dz_dy).sqrt().atan();
                let mut t = (slope / max_rad).clamp(0.0, 1.0);
                if invert {
                    t = 1.0 - t;
                }
                let g = (t * 255.0).round() as u8;
                let i = ((y * w + x) * 4) as usize;
                out.pixels[i] = g;
                out.pixels[i + 1] = g;
                out.pixels[i + 2] = g;
                out.pixels[i + 3] = 255;
            }
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"slope");
        self.max_deg.param_hash(h);
        self.invert.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct SlopeFactory;
impl NodeFactory for SlopeFactory {
    fn op_name(&self) -> &'static str {
        "slope"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "field")?;
        let mut r = InReader::new(fields, ctx, 1);
        let max_deg = r.number_or("max-deg", 60.0)?;
        let invert = r.bool_or("invert", false)?;
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
            node: Box::new(SlopeNode {
                max_deg,
                invert,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Slope angle as grayscale, normalised to 0..1 against `max-deg`.",
            "properties": {
                "field": schema_frag::node_ref(),
                "max-deg": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 60,
                              "description": "Slope angle (degrees) that maps to white." })),
                "invert": { "oneOf": [{"type": "boolean"}, {"type": "string", "pattern": "^[$@].+"}], "default": false,
                            "description": "If true, flat = white and steep = black." },
            },
            "required": ["field"],
        })
    }
}

ezu_graph::submit_node!(SlopeFactory);
