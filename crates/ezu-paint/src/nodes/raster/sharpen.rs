//! `sharpen` — `Raster|Sprite` pass-through. Classic 4-neighbour
//! Laplacian sharpen: each pixel is amplified relative to its
//! orthogonal neighbours by `amount`. With `amount = 0` it's a no-op;
//! around `1.0` it's a typical "unsharp mask" look. Grows upstream
//! pad by 1 so the 3-tap kernel stays in-bounds at tile borders.

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

struct SharpenNode {
    amount: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for SharpenNode {
    fn op_name(&self) -> &'static str {
        "sharpen"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        raster_or_sprite_output(input_kinds)
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + 1
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
        let amount = self.amount.get(ctx, inputs)? as f32;
        if amount.abs() < 1e-6 {
            return Ok(wrap_raster_like(src, kind));
        }
        let w = src.width;
        let h = src.height;
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
        self.amount.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        let mut r = InReader::new(fields, ctx, 1);
        let amount = r.number_or("amount", 0.5)?;
        let parts = r.finish();

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
            node: Box::new(SharpenNode {
                amount,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "4-neighbour Laplacian sharpen: amplifies each pixel relative to its orthogonal neighbours by `amount`. Around 0.5–1.0 is a typical unsharp-mask look; negative values give a soft halo. Grows upstream pad by 1.",
            "properties": {
                "input": schema_frag::node_ref(),
                "amount": schema_frag::in_number(serde_json::json!({
                    "type": "number", "default": 0.5,
                    "description": "Sharpening strength. 0 = pass-through, ~1 = strong."
                })),
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(SharpenFactory);
