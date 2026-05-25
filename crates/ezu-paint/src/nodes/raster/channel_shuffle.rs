//! `channel-shuffle` — `Raster|Sprite` pass-through. Rearrange (or
//! synthesise) the four output channels. Each output channel is
//! sourced by name from one of the input's channels (`r`/`g`/`b`/`a`)
//! or set to a constant (`0`/`1`). Shuffling happens in
//! non-premultiplied sRGB so the result reads as colour swaps rather
//! than weird premultiplied artefacts.
//!
//! Defaults to identity (`r→r`, `g→g`, `b→b`, `a→a`). Specify only
//! the channels you want to remap.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    raster_or_sprite_output, read_optional_string, unwrap_raster_or_sprite, wrap_raster_like,
    ACCEPTS_RASTER_OR_SPRITE,
};

#[derive(Debug, Clone, Copy)]
enum Src {
    R,
    G,
    B,
    A,
    Zero,
    One,
}

impl Src {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "r" | "R" => Some(Src::R),
            "g" | "G" => Some(Src::G),
            "b" | "B" => Some(Src::B),
            "a" | "A" => Some(Src::A),
            "0" => Some(Src::Zero),
            "1" => Some(Src::One),
            _ => None,
        }
    }
    fn pick(self, rgba: [f32; 4]) -> f32 {
        match self {
            Src::R => rgba[0],
            Src::G => rgba[1],
            Src::B => rgba[2],
            Src::A => rgba[3],
            Src::Zero => 0.0,
            Src::One => 1.0,
        }
    }
    fn tag(self) -> u8 {
        match self {
            Src::R => 0,
            Src::G => 1,
            Src::B => 2,
            Src::A => 3,
            Src::Zero => 4,
            Src::One => 5,
        }
    }
}

struct ChannelShuffleNode {
    sources: [Src; 4],
}

impl Node for ChannelShuffleNode {
    fn op_name(&self) -> &'static str {
        "channel-shuffle"
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
            let a_in = src.pixels[i + 3] as f32 / 255.0;
            // Demultiply RGB; alpha is its own channel and stays linear.
            let rgba = if a_in > 0.0 {
                [
                    (src.pixels[i] as f32 / 255.0) / a_in,
                    (src.pixels[i + 1] as f32 / 255.0) / a_in,
                    (src.pixels[i + 2] as f32 / 255.0) / a_in,
                    a_in,
                ]
            } else {
                [0.0, 0.0, 0.0, 0.0]
            };
            let r = self.sources[0].pick(rgba).clamp(0.0, 1.0);
            let g = self.sources[1].pick(rgba).clamp(0.0, 1.0);
            let b = self.sources[2].pick(rgba).clamp(0.0, 1.0);
            let a = self.sources[3].pick(rgba).clamp(0.0, 1.0);
            out.pixels[i] = (r * a * 255.0).round() as u8;
            out.pixels[i + 1] = (g * a * 255.0).round() as u8;
            out.pixels[i + 2] = (b * a * 255.0).round() as u8;
            out.pixels[i + 3] = (a * 255.0).round() as u8;
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"channel-shuffle");
        h.update(&[
            self.sources[0].tag(),
            self.sources[1].tag(),
            self.sources[2].tag(),
            self.sources[3].tag(),
        ]);
    }
}

pub(super) struct ChannelShuffleFactory;
impl NodeFactory for ChannelShuffleFactory {
    fn op_name(&self) -> &'static str {
        "channel-shuffle"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let pick = |name: &str, default: Src| -> Result<Src, FactoryError> {
            match read_optional_string(fields, name)? {
                None => Ok(default),
                Some(s) => Src::parse(&s).ok_or_else(|| FactoryError::BadField {
                    field: name.into(),
                    msg: format!("expected one of r/g/b/a/0/1, got `{s}`"),
                }),
            }
        };
        let r = pick("r", Src::R)?;
        let g = pick("g", Src::G)?;
        let b = pick("b", Src::B)?;
        let a = pick("a", Src::A)?;
        Ok(BuiltNode {
            node: Box::new(ChannelShuffleNode {
                sources: [r, g, b, a],
            }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        let choice = serde_json::json!({
            "type": "string",
            "enum": ["r", "g", "b", "a", "0", "1"],
        });
        serde_json::json!({
            "description": "Rearrange RGBA channels. Each output (`r`/`g`/`b`/`a`) names which input channel feeds it, or a constant (`0`/`1`). Identity by default. Operates in non-premultiplied sRGB.",
            "properties": {
                "input": schema_frag::node_ref(),
                "r": choice.clone(),
                "g": choice.clone(),
                "b": choice.clone(),
                "a": choice,
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(ChannelShuffleFactory);
