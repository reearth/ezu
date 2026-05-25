//! `brightness-contrast` — `Raster -> Raster`. Linear brightness shift
//! and contrast slope around mid-gray. Operates in non-premultiplied
//! sRGB; alpha is preserved.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::read_number_or;

struct BrightnessContrastNode {
    brightness: f32,
    contrast: f32,
}

impl Node for BrightnessContrastNode {
    fn op_name(&self) -> &'static str {
        "brightness-contrast"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "input",
            accepts: &[PortKind::Raster],
            optional: false,
        }];
        SPECS
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let src = inputs[0]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let slope = 1.0 + self.contrast;
        let offset = self.brightness;
        let mut out = RasterBuf::new(src.width, src.height);
        for i in (0..src.pixels.len()).step_by(4) {
            let a = src.pixels[i + 3] as f32 / 255.0;
            if a <= 0.0 {
                continue; // transparent stays transparent (zeroed)
            }
            let r = (src.pixels[i] as f32 / 255.0) / a;
            let g = (src.pixels[i + 1] as f32 / 255.0) / a;
            let b = (src.pixels[i + 2] as f32 / 255.0) / a;
            let nr = ((r - 0.5) * slope + 0.5 + offset).clamp(0.0, 1.0);
            let ng = ((g - 0.5) * slope + 0.5 + offset).clamp(0.0, 1.0);
            let nb = ((b - 0.5) * slope + 0.5 + offset).clamp(0.0, 1.0);
            out.pixels[i] = (nr * a * 255.0).round() as u8;
            out.pixels[i + 1] = (ng * a * 255.0).round() as u8;
            out.pixels[i + 2] = (nb * a * 255.0).round() as u8;
            out.pixels[i + 3] = src.pixels[i + 3];
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"brightness-contrast");
        h.update(&self.brightness.to_le_bytes());
        h.update(&self.contrast.to_le_bytes());
    }
}

pub(super) struct BrightnessContrastFactory;
impl NodeFactory for BrightnessContrastFactory {
    fn op_name(&self) -> &'static str {
        "brightness-contrast"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let brightness = read_number_or(fields, "brightness", ctx, 0.0)? as f32;
        let contrast = read_number_or(fields, "contrast", ctx, 0.0)? as f32;
        Ok(BuiltNode {
            node: Box::new(BrightnessContrastNode {
                brightness,
                contrast,
            }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Linear brightness shift and contrast slope around mid-gray. Both in [-1, 1], 0 = no change.",
            "properties": {
                "input": schema_frag::node_ref(),
                "brightness": { "type": "number", "minimum": -1.0, "maximum": 1.0, "default": 0.0 },
                "contrast": { "type": "number", "minimum": -1.0, "maximum": 1.0, "default": 0.0 },
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(BrightnessContrastFactory);
