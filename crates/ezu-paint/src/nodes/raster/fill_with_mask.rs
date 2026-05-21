//! `fill-with-mask` — `Mask + scalar Color -> Raster`. Tints a mask
//! with a color, producing a premultiplied raster.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::read_color;

struct FillWithMaskNode {
    color: [f32; 4],
}

impl Node for FillWithMaskNode {
    fn op_name(&self) -> &'static str {
        "fill-with-mask"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "mask",
            kind: PortKind::Mask,
            optional: false,
        }];
        SPECS
    }
    fn output(&self) -> PortKind {
        PortKind::Raster
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let mask = inputs[0]
            .as_ref()
            .and_then(PortValue::as_mask)
            .ok_or_else(|| EvalError::MissingInput("mask".into()))?;
        let mut out = RasterBuf::new(mask.width, mask.height);
        let [r, g, b, a] = self.color;
        for i in 0..mask.pixels.len() {
            let m = mask.pixels[i].clamp(0.0, 1.0);
            let alpha = a * m;
            let pr = r * alpha;
            let pg = g * alpha;
            let pb = b * alpha;
            let o = i * 4;
            out.pixels[o] = (pr * 255.0).round() as u8;
            out.pixels[o + 1] = (pg * 255.0).round() as u8;
            out.pixels[o + 2] = (pb * 255.0).round() as u8;
            out.pixels[o + 3] = (alpha * 255.0).round() as u8;
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"fill-with-mask");
        for c in self.color {
            h.update(&c.to_le_bytes());
        }
    }
}

pub(super) struct FillWithMaskFactory;
impl NodeFactory for FillWithMaskFactory {
    fn op_name(&self) -> &'static str { "fill-with-mask" }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let mask = take_input_ref(fields, "mask")?;
        let color = read_color(fields, "color", ctx)?;
        Ok(BuiltNode {
            node: Box::new(FillWithMaskNode { color }),
            connections: vec![Connection {
                port: "mask".into(),
                src: mask,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Tint a mask with a solid color, producing a premultiplied raster.",
            "properties": {
                "mask": schema_frag::node_ref(),
                "color": schema_frag::color(),
            },
            "required": ["mask", "color"],
        })
    }
}

ezu_graph::submit_node!(FillWithMaskFactory);
