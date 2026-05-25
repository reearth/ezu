//! `sharpen` — `Raster|Sprite` pass-through. Classic 4-neighbour
//! Laplacian sharpen: each pixel is amplified relative to its
//! orthogonal neighbours by `amount`. With `amount = 0` it's a no-op;
//! around `1.0` it's a typical "unsharp mask" look. Grows upstream
//! pad by 1 so the 3-tap kernel stays in-bounds at tile borders.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    raster_or_sprite_output, read_number_or, unwrap_raster_or_sprite, wrap_raster_like,
    ACCEPTS_RASTER_OR_SPRITE,
};

struct SharpenNode {
    amount: f32,
}

impl Node for SharpenNode {
    fn op_name(&self) -> &'static str {
        "sharpen"
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
        downstream + 1
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
        if self.amount.abs() < 1e-6 {
            return Ok(wrap_raster_like(src, kind));
        }
        let w = src.width;
        let h = src.height;
        let amount = self.amount;
        // Convolution with the cross Laplacian:
        //     0  -k   0
        //    -k 1+4k -k
        //     0  -k   0
        // Applied per channel on premultiplied data, then clamped.
        // Premultiplied is fine here: the kernel is a linear filter
        // and stays consistent across alpha values.
        let sample = |x: i32, y: i32, c: usize| -> i32 {
            let xc = x.clamp(0, w as i32 - 1) as u32;
            let yc = y.clamp(0, h as i32 - 1) as u32;
            src.pixels[((yc * w + xc) * 4) as usize + c] as i32
        };
        let mut out = RasterBuf::new(w, h);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let off = ((y as u32 * w + x as u32) * 4) as usize;
                for c in 0..4 {
                    let centre = sample(x, y, c) as f32;
                    let neigh = sample(x - 1, y, c) as f32
                        + sample(x + 1, y, c) as f32
                        + sample(x, y - 1, c) as f32
                        + sample(x, y + 1, c) as f32;
                    let v = centre * (1.0 + 4.0 * amount) - neigh * amount;
                    out.pixels[off + c] = v.clamp(0.0, 255.0) as u8;
                }
            }
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"sharpen");
        h.update(&self.amount.to_le_bytes());
    }
}

pub(super) struct SharpenFactory;
impl NodeFactory for SharpenFactory {
    fn op_name(&self) -> &'static str {
        "sharpen"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let amount = read_number_or(fields, "amount", ctx, 0.5)? as f32;
        Ok(BuiltNode {
            node: Box::new(SharpenNode { amount }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "4-neighbour Laplacian sharpen: amplifies each pixel relative to its orthogonal neighbours by `amount`. Around 0.5–1.0 is a typical unsharp-mask look; negative values give a soft halo. Grows upstream pad by 1.",
            "properties": {
                "input": schema_frag::node_ref(),
                "amount": { "type": "number", "default": 0.5,
                            "description": "Sharpening strength. 0 = pass-through, ~1 = strong." },
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(SharpenFactory);
