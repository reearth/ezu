//! `saturate` and `vibrance` — `Raster|Sprite -> Raster|Sprite`. Adjust
//! colourfulness by scaling **CIELAB chroma**, so hue and perceived
//! lightness stay put (unlike an HSL-saturation tweak, which shifts both).
//!
//! - `saturate`: uniform chroma scale by `amount` (1 = identity, 0 = grey,
//!   >1 = punchier).
//! - `vibrance`: adaptive — boosts low-chroma pixels more than already-
//!   saturated ones (`amount` = strength, 0 = identity), the classic
//!   "vibrance" that protects skin tones / avoids over-saturating.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::color_interp::{lab_to_rgb, rgb_to_lab};
use crate::nodes::common::{
    raster_or_sprite_output, unwrap_raster_or_sprite, wrap_raster_like, ACCEPTS_RASTER_OR_SPRITE,
};

/// Reference chroma for normalising "how saturated" a pixel already is.
/// LAB chroma tops out around 130 for the most vivid sRGB colours.
const CHROMA_REF: f32 = 100.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Saturate,
    Vibrance,
}

struct ChromaNode {
    mode: Mode,
    amount: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for ChromaNode {
    fn op_name(&self) -> &'static str {
        match self.mode {
            Mode::Saturate => "saturate",
            Mode::Vibrance => "vibrance",
        }
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
        let amount = self.amount.get(ctx, inputs)? as f32;
        let mut out = RasterBuf::new(src.width, src.height);
        for i in (0..src.pixels.len()).step_by(4) {
            let a = src.pixels[i + 3] as f32 / 255.0;
            if a <= 0.0 {
                continue;
            }
            let rgb = [
                (src.pixels[i] as f32 / 255.0 / a).min(1.0),
                (src.pixels[i + 1] as f32 / 255.0 / a).min(1.0),
                (src.pixels[i + 2] as f32 / 255.0 / a).min(1.0),
                1.0,
            ];
            let mut lab = rgb_to_lab(rgb);
            let scale = match self.mode {
                Mode::Saturate => amount.max(0.0),
                Mode::Vibrance => {
                    // Boost inversely to current chroma: grey pixels move
                    // most, vivid pixels barely.
                    let chroma = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
                    let sat = (chroma / CHROMA_REF).clamp(0.0, 1.0);
                    (1.0 + amount * (1.0 - sat)).max(0.0)
                }
            };
            lab[1] *= scale;
            lab[2] *= scale;
            let c = lab_to_rgb(lab);
            out.pixels[i] = (c[0] * a * 255.0).round() as u8;
            out.pixels[i + 1] = (c[1] * a * 255.0).round() as u8;
            out.pixels[i + 2] = (c[2] * a * 255.0).round() as u8;
            out.pixels[i + 3] = src.pixels[i + 3];
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(match self.mode {
            Mode::Saturate => b"saturate",
            Mode::Vibrance => b"vibrance",
        });
        self.amount.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

/// Shared build for both ops: one `input` port + a scalar `amount`.
fn build_chroma(
    mode: Mode,
    default_amount: f64,
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
) -> Result<BuiltNode, FactoryError> {
    let input = take_input_ref(fields, "input")?;
    let mut r = InReader::new(fields, ctx, 1);
    let amount = r.number_or("amount", default_amount)?;
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
        node: Box::new(ChromaNode {
            mode,
            amount,
            ports,
            param_refs: parts.param_refs,
        }),
        connections,
    })
}

fn chroma_schema(desc: &str, default: f64) -> Value {
    serde_json::json!({
        "description": desc,
        "properties": {
            "input": schema_frag::node_ref(),
            "amount": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "default": default })),
        },
        "required": ["input"],
    })
}

pub(super) struct SaturateFactory;
impl NodeFactory for SaturateFactory {
    fn op_name(&self) -> &'static str {
        "saturate"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        build_chroma(Mode::Saturate, 1.0, fields, ctx)
    }
    fn schema(&self) -> Value {
        chroma_schema(
            "Scale CIELAB chroma uniformly by `amount` (1 = identity, 0 = greyscale, >1 = more saturated). Hue and lightness are preserved.",
            1.0,
        )
    }
}

pub(super) struct VibranceFactory;
impl NodeFactory for VibranceFactory {
    fn op_name(&self) -> &'static str {
        "vibrance"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        build_chroma(Mode::Vibrance, 0.0, fields, ctx)
    }
    fn schema(&self) -> Value {
        chroma_schema(
            "Adaptively boost CIELAB chroma — low-chroma pixels rise more than already-vivid ones (`amount` = strength, 0 = identity). Hue and lightness preserved.",
            0.0,
        )
    }
}

ezu_graph::submit_node!(SaturateFactory);
ezu_graph::submit_node!(VibranceFactory);
