//! `pick-channel` — `Raster -> ScalarField`. Extract a single channel
//! from an RGBA raster (or its luminance) as a `[0, 1]` scalar field.
//! Bridges the raster pipeline into scalar-field ops (`map-range`,
//! `threshold`, `color-ramp`).
//!
//! RGB channels are read in **non-premultiplied** space — pure red
//! at full alpha gives R = 1.0 regardless of the alpha. The alpha
//! channel is read directly. `luminance` is a Rec. 601 luma
//! (0.299 R + 0.587 G + 0.114 B) over the non-premultiplied colour.
//!
//! Only accepts canvas-padded `Raster` (not `Sprite`) — the output
//! ScalarField inherits the input dimensions, and downstream scalar
//! consumers expect canvas-padded sizing.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EnumTable, EvalCtx, EvalError, FactoryCtx,
    FactoryError, InEnum, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Channel {
    R,
    G,
    B,
    A,
    Luminance,
}

/// The schema's names first, then the other spellings accepted for each.
const CHANNELS: EnumTable<Channel> = &[
    ("r", Channel::R),
    ("g", Channel::G),
    ("b", Channel::B),
    ("a", Channel::A),
    ("luminance", Channel::Luminance),
    ("R", Channel::R),
    ("red", Channel::R),
    ("G", Channel::G),
    ("green", Channel::G),
    ("B", Channel::B),
    ("blue", Channel::B),
    ("A", Channel::A),
    ("alpha", Channel::A),
    ("luma", Channel::Luminance),
    ("y", Channel::Luminance),
];

struct PickChannelNode {
    channel: InEnum<Channel>,
    param_refs: Vec<String>,
}

impl Node for PickChannelNode {
    fn op_name(&self) -> &'static str {
        "pick-channel"
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
        PortKind::ScalarField
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let src = inputs[0]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let channel = self.channel.get(ctx)?;
        let w = src.width;
        let h = src.height;
        let count = (w * h) as usize;
        let mut values: Vec<f32> = Vec::with_capacity(count);
        for i in (0..src.pixels.len()).step_by(4) {
            let a = src.pixels[i + 3] as f32 / 255.0;
            let v = match channel {
                Channel::A => a,
                Channel::R | Channel::G | Channel::B | Channel::Luminance => {
                    if a <= 0.0 {
                        0.0
                    } else {
                        let r = (src.pixels[i] as f32 / 255.0) / a;
                        let g = (src.pixels[i + 1] as f32 / 255.0) / a;
                        let b = (src.pixels[i + 2] as f32 / 255.0) / a;
                        match channel {
                            Channel::R => r,
                            Channel::G => g,
                            Channel::B => b,
                            Channel::Luminance => 0.299 * r + 0.587 * g + 0.114 * b,
                            Channel::A => unreachable!(),
                        }
                    }
                }
            };
            values.push(v.clamp(0.0, 1.0));
        }
        Ok(PortValue::ScalarField(Arc::new(ScalarField {
            width: w,
            height: h,
            values: values.into(),
            nodata: None,
            // The extracted channel is unitless [0, 1], not a
            // geographically scaled quantity.
            geo_scale: None,
        })))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"pick-channel");
        self.channel.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct PickChannelFactory;
impl NodeFactory for PickChannelFactory {
    fn op_name(&self) -> &'static str {
        "pick-channel"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let mut r = InReader::new(fields, ctx, 1);
        let channel = r.enum_or("channel", Channel::Luminance, CHANNELS)?;
        Ok(BuiltNode {
            node: Box::new(PickChannelNode {
                channel,
                param_refs: r.finish().param_refs,
            }),
            connections: vec![Connection {
                port: "input".into(),
                src: input,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Extract a single channel of an RGBA raster as a [0, 1] ScalarField. RGB channels are read in non-premultiplied space; `luminance` is Rec. 601 luma over the non-premultiplied colour. Bridges the raster pipeline into scalar-field ops (`map-range`, `threshold`, `color-ramp`).",
            "properties": {
                "input": schema_frag::node_ref(),
                "channel": schema_frag::in_enum(serde_json::json!({
                    "type": "string",
                    "enum": ["r", "g", "b", "a", "luminance"],
                    "default": "luminance",
                })),
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(PickChannelFactory);
