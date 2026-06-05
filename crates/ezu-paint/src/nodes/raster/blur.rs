//! `blur` — Gaussian blur (libblur, separable exact). Pass-through
//! over `Raster` and `Sprite`: the output port kind mirrors the input.
//! Grows upstream pad by 3σ (only meaningful for `Raster` inputs;
//! `Sprite` producers ignore pad).
//!
//! `sigma` is an `In<f64>` field, but pad is computed at build time —
//! so it must carry a static upper bound: a literal, or a `$param`
//! whose declaration has `max`. Wiring sigma from a `@node` port is
//! rejected at build time.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    raster_or_sprite_output, unwrap_raster_or_sprite, wrap_raster_like, ACCEPTS_RASTER_OR_SPRITE,
};

struct BlurNode {
    sigma: In<f64>,
    /// Build-time upper bound on sigma, for pad propagation.
    sigma_bound: f32,
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
        raster_or_sprite_output(input_kinds)
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + (3.0 * self.sigma_bound).ceil() as u32
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let (src, kind) = unwrap_raster_or_sprite(input, "input")?;
        let sigma = self.sigma.get(ctx, inputs)? as f32;
        if sigma <= 0.0 {
            return Ok(wrap_raster_like(src, kind));
        }
        let mut out = RasterBuf::new(src.width, src.height);
        // RasterBuf is premultiplied RGBA8; blurring premultiplied data
        // directly is the mathematically correct path (avoids halos at
        // transparent edges).
        let src_view = libblur::BlurImage::borrow(
            &src.pixels,
            src.width,
            src.height,
            libblur::FastBlurChannels::Channels4,
        );
        let mut dst_view = libblur::BlurImageMut::borrow(
            &mut out.pixels,
            src.width,
            src.height,
            libblur::FastBlurChannels::Channels4,
        );
        let _ = libblur::gaussian_blur(
            &src_view,
            &mut dst_view,
            libblur::GaussianBlurParams::new_from_sigma(sigma as f64),
            libblur::EdgeMode2D::new(libblur::EdgeMode::Clamp),
            libblur::ThreadingPolicy::Single,
            libblur::ConvolutionMode::Exact,
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
        let sigma = r.number("sigma")?;
        let parts = r.finish();
        let sigma_bound = sigma.static_bound().ok_or_else(|| FactoryError::BadField {
            field: "sigma".into(),
            msg: "pad depends on sigma at build time: use a literal, or a `$param` with `max` \
                  (a `@node` port has no static bound)"
                .into(),
        })? as f32;

        let mut ports = vec![PortSpec {
            name: "input",
            accepts: ACCEPTS_RASTER_OR_SPRITE,
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
                sigma_bound,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Gaussian blur on a raster (libblur, exact). Grows upstream pad by 3σ; a `$param` sigma needs a declared `max`.",
            "properties": {
                "input": schema_frag::node_ref(),
                "sigma": schema_frag::px_number(),
            },
            "required": ["input", "sigma"],
        })
    }
}

ezu_graph::submit_node!(BlurFactory);
