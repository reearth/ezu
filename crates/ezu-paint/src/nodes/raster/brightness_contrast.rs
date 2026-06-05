//! `brightness-contrast` — linear brightness shift and contrast slope
//! around mid-gray over `Raster|Sprite` (pass-through). Operates in
//! non-premultiplied sRGB; alpha is preserved.

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

struct BrightnessContrastNode {
    brightness: In<f64>,
    contrast: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for BrightnessContrastNode {
    fn op_name(&self) -> &'static str {
        "brightness-contrast"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        raster_or_sprite_output(input_kinds)
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
        let brightness = self.brightness.get(ctx, inputs)? as f32;
        let contrast = self.contrast.get(ctx, inputs)? as f32;
        let slope = 1.0 + contrast;
        let offset = brightness;
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
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"brightness-contrast");
        self.brightness.param_hash(h);
        self.contrast.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        let mut r = InReader::new(fields, ctx, 1);
        let brightness = r.number_or("brightness", 0.0)?;
        let contrast = r.number_or("contrast", 0.0)?;
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
            node: Box::new(BrightnessContrastNode {
                brightness,
                contrast,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Linear brightness shift and contrast slope around mid-gray. Both in [-1, 1], 0 = no change.",
            "properties": {
                "input": schema_frag::node_ref(),
                "brightness": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": -1.0, "maximum": 1.0, "default": 0.0 })),
                "contrast": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": -1.0, "maximum": 1.0, "default": 0.0 })),
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(BrightnessContrastFactory);
