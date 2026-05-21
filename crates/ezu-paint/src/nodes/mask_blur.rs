//! `mask-blur` — `Mask -> Mask`. Separable Gaussian blur; grows
//! upstream pad by 3σ.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, MaskBuf, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::common::read_number;

struct MaskBlurNode {
    sigma: f32,
}

impl Node for MaskBlurNode {
    fn op_name(&self) -> &'static str {
        "mask-blur"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "input",
            kind: PortKind::Mask,
            optional: false,
        }];
        SPECS
    }
    fn output(&self) -> PortKind {
        PortKind::Mask
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + (3.0 * self.sigma).ceil() as u32
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let src = inputs[0]
            .as_ref()
            .and_then(PortValue::as_mask)
            .ok_or_else(|| EvalError::MissingInput("input".into()))?
            .clone();
        let out = gaussian_blur_mask(&src, self.sigma);
        Ok(PortValue::Mask(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"mask-blur");
        h.update(&self.sigma.to_le_bytes());
    }
}

pub(super) struct MaskBlurFactory;
impl NodeFactory for MaskBlurFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let sigma = read_number(fields, "sigma", ctx)? as f32;
        Ok(BuiltNode {
            node: Box::new(MaskBlurNode { sigma }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Separable Gaussian blur on a mask. Grows upstream pad by 3σ.",
            "properties": {
                "input": schema_frag::node_ref(),
                "sigma": schema_frag::px_number(),
            },
            "required": ["input", "sigma"],
        })
    }
}

// ---------------------------------------------------------------------------
// Two-pass separable Gaussian on `MaskBuf`. Private to this module.

fn gaussian_blur_mask(src: &MaskBuf, sigma: f32) -> MaskBuf {
    if sigma <= 0.0 {
        return src.clone();
    }
    let kernel = gaussian_kernel(sigma);
    let kh = (kernel.len() / 2) as i32;
    let w = src.width as i32;
    let h = src.height as i32;
    // Horizontal pass.
    let mut tmp = vec![0.0f32; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0.0;
            for (i, k) in kernel.iter().enumerate() {
                let sx = (x + i as i32 - kh).clamp(0, w - 1);
                sum += k * src.pixels[(y * w + sx) as usize];
            }
            tmp[(y * w + x) as usize] = sum;
        }
    }
    // Vertical pass.
    let mut out = MaskBuf::new(src.width, src.height);
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0.0;
            for (i, k) in kernel.iter().enumerate() {
                let sy = (y + i as i32 - kh).clamp(0, h - 1);
                sum += k * tmp[(sy * w + x) as usize];
            }
            out.pixels[(y * w + x) as usize] = sum;
        }
    }
    out
}

fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let radius = (3.0 * sigma).ceil() as i32;
    let len = (2 * radius + 1) as usize;
    let mut k = Vec::with_capacity(len);
    let two_s2 = 2.0 * sigma * sigma;
    let mut sum = 0.0;
    for i in -radius..=radius {
        let v = (-(i as f32 * i as f32) / two_s2).exp();
        k.push(v);
        sum += v;
    }
    for v in &mut k {
        *v /= sum;
    }
    k
}
