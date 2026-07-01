//! `quantize` — `Raster|Sprite -> Raster|Sprite`. Snap every pixel to the
//! nearest colour in a fixed `palette`, measuring distance perceptually in
//! **CIELAB** (ΔE, the default) or in plain RGB. Source coverage (alpha) is
//! preserved. Great for limited-palette / poster / pixel-art looks — where
//! `posterize` (independent per-channel quantization) can't snap to a
//! chosen set of colours. See `dither` for the error-diffused variant.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    raster_or_sprite_output, unwrap_raster_or_sprite, wrap_raster_like, ACCEPTS_RASTER_OR_SPRITE,
};
use crate::nodes::raster::palette::Palette;

struct QuantizeNode {
    palette: Palette,
}

impl Node for QuantizeNode {
    fn op_name(&self) -> &'static str {
        "quantize"
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
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let (src, kind) = unwrap_raster_or_sprite(input, "input")?;
        let mut out = RasterBuf::new(src.width, src.height);
        for i in (0..src.pixels.len()).step_by(4) {
            let a = src.pixels[i + 3] as f32 / 255.0;
            if a <= 0.0 {
                continue; // stays transparent
            }
            let rgb = [
                (src.pixels[i] as f32 / 255.0 / a).min(1.0),
                (src.pixels[i + 1] as f32 / 255.0 / a).min(1.0),
                (src.pixels[i + 2] as f32 / 255.0 / a).min(1.0),
            ];
            let q = self.palette.nearest(rgb);
            // Re-premultiply with the source alpha (preserve coverage).
            out.pixels[i] = (q[0] * a * 255.0).round() as u8;
            out.pixels[i + 1] = (q[1] * a * 255.0).round() as u8;
            out.pixels[i + 2] = (q[2] * a * 255.0).round() as u8;
            out.pixels[i + 3] = src.pixels[i + 3];
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"quantize");
        self.palette.hash(h);
    }
}

pub(super) struct QuantizeFactory;
impl NodeFactory for QuantizeFactory {
    fn op_name(&self) -> &'static str {
        "quantize"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let palette = Palette::from_fields(fields, ctx)?;
        Ok(BuiltNode {
            node: Box::new(QuantizeNode { palette }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Snap each pixel to the nearest colour in `palette`, measuring distance in `space` (`lab` = perceptual ΔE, default; or `rgb`). Alpha is preserved. Use for limited-palette / poster / pixel-art looks; see `dither` for the error-diffused variant.",
            "properties": {
                "input": schema_frag::node_ref(),
                "palette": {
                    "type": "array",
                    "minItems": 1,
                    "items": { "type": "string", "description": "`#rrggbb` or `#rrggbbaa` (alpha ignored for matching)." },
                },
                "space": { "type": "string", "enum": ["lab", "rgb"], "default": "lab", "description": "Colour space the nearest-colour distance is measured in." },
            },
            "required": ["input", "palette"],
        })
    }
}

ezu_graph::submit_node!(QuantizeFactory);
