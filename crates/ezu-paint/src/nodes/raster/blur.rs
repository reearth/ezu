//! `blur` — Gaussian blur (libblur, separable exact). Pass-through
//! over `Raster` and `Sprite`: the output port kind mirrors the input.
//! Grows upstream pad by 3σ (only meaningful for `Raster` inputs;
//! `Sprite` producers ignore pad).

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    raster_or_sprite_output, read_number, unwrap_raster_or_sprite, wrap_raster_like,
    ACCEPTS_RASTER_OR_SPRITE,
};

struct BlurNode {
    sigma: f32,
}

impl Node for BlurNode {
    fn op_name(&self) -> &'static str {
        "blur"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "input",
            accepts: ACCEPTS_RASTER_OR_SPRITE,
            optional: false,
        }];
        SPECS
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        raster_or_sprite_output(input_kinds)
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + (3.0 * self.sigma).ceil() as u32
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let (src, kind) = unwrap_raster_or_sprite(input, "input")?;
        if self.sigma <= 0.0 {
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
            libblur::GaussianBlurParams::new_from_sigma(self.sigma as f64),
            libblur::EdgeMode2D::new(libblur::EdgeMode::Clamp),
            libblur::ThreadingPolicy::Single,
            libblur::ConvolutionMode::Exact,
        );
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"blur");
        h.update(&self.sigma.to_le_bytes());
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
        let sigma = read_number(fields, "sigma", ctx)? as f32;
        Ok(BuiltNode {
            node: Box::new(BlurNode { sigma }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Gaussian blur on a raster (libblur, exact). Grows upstream pad by 3σ.",
            "properties": {
                "input": schema_frag::node_ref(),
                "sigma": schema_frag::px_number(),
            },
            "required": ["input", "sigma"],
        })
    }
}

ezu_graph::submit_node!(BlurFactory);
