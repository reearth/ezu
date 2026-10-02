//! `blur` — Gaussian blur (separable). Pass-through over `Raster`,
//! `Sprite` and `ScalarField`: the output port kind mirrors the input.
//! Grows upstream pad by 3σ (meaningful for canvas-sized `Raster` and
//! `ScalarField` inputs; `Sprite` producers ignore pad).
//!
//! Images are blurred in fixed point. A `ScalarField` is blurred in
//! `f32`, leaving out `nodata` samples, and keeps its `nodata` and
//! `geo_scale`: blurring an elevation field before `hillshade`, `slope`
//! or `flow-field` generalises the terrain, dropping small gullies and
//! keeping the major ridges, which blurring their output cannot do.
//!
//! `sigma` is an `In<f64>` field, but pad is computed at build time —
//! so it must carry a static upper bound: a literal, or a `$param`
//! whose declaration has `max`. Wiring sigma from a `@node` port is
//! rejected at build time.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, InReader, Node, NodeFactory, PaddingIn, PortKind, PortSpec, PortValue, RasterBuf,
    ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    image_or_field_output, unwrap_raster_or_sprite, wrap_raster_like, ACCEPTS_IMAGE_OR_FIELD,
};

struct BlurNode {
    sigma: PaddingIn,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for BlurNode {
    fn op_name(&self) -> &'static str {
        "blur"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        image_or_field_output(input_kinds)
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + (3.0 * self.sigma.bound() as f32).ceil() as u32
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let sigma = self.sigma.get(ctx, inputs)? as f32;
        if let Some(field) = input.as_scalar_field() {
            if sigma <= 0.0 {
                return Ok(input.clone());
            }
            let mut values = vec![0f32; field.values.len()];
            crate::imaging::gaussian_blur_field(
                &field.values,
                &mut values,
                field.width as usize,
                field.height as usize,
                sigma,
                field.nodata,
            );
            return Ok(PortValue::ScalarField(Arc::new(ScalarField {
                values: values.into(),
                ..**field
            })));
        }
        let (src, kind) = unwrap_raster_or_sprite(input, "input")?;
        if sigma <= 0.0 {
            return Ok(wrap_raster_like(src, kind));
        }
        let mut out = RasterBuf::new(src.width, src.height);
        // RasterBuf is premultiplied RGBA8; blurring premultiplied data
        // directly is the mathematically correct path (avoids halos at
        // transparent edges).
        crate::imaging::gaussian_blur_premultiplied(
            &src.pixels,
            &mut out.pixels,
            src.width as usize,
            src.height as usize,
            sigma,
        );
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"blur");
        self.sigma.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct BlurFactory;
impl NodeFactory for BlurFactory {
    fn op_name(&self) -> &'static str {
        "blur"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let mut r = InReader::new(fields, ctx, 1);
        let sigma = PaddingIn::read(&mut r, fields, "sigma")?;
        let parts = r.finish();

        let mut ports = vec![PortSpec {
            name: "input",
            accepts: ACCEPTS_IMAGE_OR_FIELD,
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "input".into(),
            src: input,
        }];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(BlurNode {
                sigma,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Gaussian blur on a raster, sprite or scalar field; the output is the same kind as the input. Rasters are blurred in fixed point and fields in a fixed order of float operations, so both render the same bytes on every host. Blurring an elevation field (`dem`) before `hillshade`, `slope` or `flow-field` generalises the terrain: small gullies drop out and the major ridges stay, so the shading follows the ridge crests. A field keeps its nodata value and geographic scale; nodata samples are left out of the average, and a pixel with nothing but nodata in reach stays nodata. Grows upstream pad by 3σ, so `sigma` needs an upper bound the build can see: a literal, a `$param` with `max`, or `sigma-max` alongside an `@node` port (the port's value is then clamped to it).",
            "properties": {
                "input": schema_frag::node_ref(),
                "sigma": schema_frag::px_number(),
                "sigma-max": { "type": "number", "minimum": 0.0, "description": "Upper bound on `sigma` for padding, required when `sigma` is an `@node` port. Values above it are clamped." },
            },
            "required": ["input", "sigma"],
        })
    }
}

ezu_graph::submit_node!(BlurFactory);
