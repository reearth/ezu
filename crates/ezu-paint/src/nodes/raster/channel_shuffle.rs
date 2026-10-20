//! `channel-shuffle` — `Raster|Sprite` pass-through. Rearrange (or
//! synthesise) the four output channels. Each output channel is
//! sourced by name from one of the input's channels (`r`/`g`/`b`/`a`),
//! from its `luminance`, or set to a constant (`0`/`1`). Luminance is
//! the Rec. 601 luma `pick-channel` reads, so a grey made here and a
//! field picked there agree. Shuffling happens in
//! non-premultiplied sRGB so the result reads as colour swaps rather
//! than weird premultiplied artefacts.
//!
//! Defaults to identity (`r→r`, `g→g`, `b→b`, `a→a`). Specify only
//! the channels you want to remap.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EnumTable, EvalCtx, EvalError, FactoryCtx,
    FactoryError, InEnum, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    raster_or_sprite_output, unwrap_raster_or_sprite, wrap_raster_like, ACCEPTS_RASTER_OR_SPRITE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Src {
    R,
    G,
    B,
    A,
    Luminance,
    Zero,
    One,
}

/// The schema's names first, then the upper-case channel letters and the
/// two other names for luminance.
const SOURCES: EnumTable<Src> = &[
    ("r", Src::R),
    ("g", Src::G),
    ("b", Src::B),
    ("a", Src::A),
    ("luminance", Src::Luminance),
    ("0", Src::Zero),
    ("1", Src::One),
    ("R", Src::R),
    ("G", Src::G),
    ("B", Src::B),
    ("A", Src::A),
    ("luma", Src::Luminance),
    ("y", Src::Luminance),
];

impl Src {
    fn pick(self, rgba: [f32; 4]) -> f32 {
        match self {
            Src::R => rgba[0],
            Src::G => rgba[1],
            Src::B => rgba[2],
            Src::A => rgba[3],
            // The same weights, in the same order, as `pick-channel`.
            Src::Luminance => 0.299 * rgba[0] + 0.587 * rgba[1] + 0.114 * rgba[2],
            Src::Zero => 0.0,
            Src::One => 1.0,
        }
    }
}

struct ChannelShuffleNode {
    sources: [InEnum<Src>; 4],
    param_refs: Vec<String>,
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
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let (src, kind) = unwrap_raster_or_sprite(input, "input")?;
        let sources = [
            self.sources[0].get(ctx)?,
            self.sources[1].get(ctx)?,
            self.sources[2].get(ctx)?,
            self.sources[3].get(ctx)?,
        ];
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
            let r = sources[0].pick(rgba).clamp(0.0, 1.0);
            let g = sources[1].pick(rgba).clamp(0.0, 1.0);
            let b = sources[2].pick(rgba).clamp(0.0, 1.0);
            let a = sources[3].pick(rgba).clamp(0.0, 1.0);
            out.pixels[i] = (r * a * 255.0).round() as u8;
            out.pixels[i + 1] = (g * a * 255.0).round() as u8;
            out.pixels[i + 2] = (b * a * 255.0).round() as u8;
            out.pixels[i + 3] = (a * 255.0).round() as u8;
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"channel-shuffle");
        for s in &self.sources {
            s.param_hash(h);
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let mut rd = InReader::new(fields, ctx, 1);
        let r = rd.enum_or("r", Src::R, SOURCES)?;
        let g = rd.enum_or("g", Src::G, SOURCES)?;
        let b = rd.enum_or("b", Src::B, SOURCES)?;
        let a = rd.enum_or("a", Src::A, SOURCES)?;
        Ok(BuiltNode {
            node: Box::new(ChannelShuffleNode {
                sources: [r, g, b, a],
                param_refs: rd.finish().param_refs,
            }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        let choice = schema_frag::in_enum(serde_json::json!({
            "type": "string",
            "enum": ["r", "g", "b", "a", "luminance", "0", "1"],
        }));
        serde_json::json!({
            "description": "Rearrange RGBA channels. Each output (`r`/`g`/`b`/`a`) names which input channel feeds it, the input's `luminance` (Rec. 601 luma, as `pick-channel` reads it), or a constant (`0`/`1`). Identity by default. Operates in non-premultiplied sRGB.",
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
