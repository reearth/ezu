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
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::read_optional_string;

#[derive(Debug, Clone, Copy)]
enum Channel {
    R,
    G,
    B,
    A,
    Luminance,
}

impl Channel {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "r" | "R" | "red" => Some(Channel::R),
            "g" | "G" | "green" => Some(Channel::G),
            "b" | "B" | "blue" => Some(Channel::B),
            "a" | "A" | "alpha" => Some(Channel::A),
            "luminance" | "luma" | "y" => Some(Channel::Luminance),
            _ => None,
        }
    }
    fn tag(self) -> u8 {
        match self {
            Channel::R => 0,
            Channel::G => 1,
            Channel::B => 2,
            Channel::A => 3,
            Channel::Luminance => 4,
        }
    }
}

struct PickChannelNode {
    channel: Channel,
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
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let src = inputs[0]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let w = src.width;
        let h = src.height;
        let count = (w * h) as usize;
        let mut values: Vec<f32> = Vec::with_capacity(count);
        for i in (0..src.pixels.len()).step_by(4) {
            let a = src.pixels[i + 3] as f32 / 255.0;
            let v = match self.channel {
                Channel::A => a,
                Channel::R | Channel::G | Channel::B | Channel::Luminance => {
                    if a <= 0.0 {
                        0.0
                    } else {
                        let r = (src.pixels[i] as f32 / 255.0) / a;
                        let g = (src.pixels[i + 1] as f32 / 255.0) / a;
                        let b = (src.pixels[i + 2] as f32 / 255.0) / a;
                        match self.channel {
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
        h.update(&[self.channel.tag()]);
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
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let channel = match read_optional_string(fields, "channel")?.as_deref() {
            None | Some("luminance") | Some("luma") | Some("y") => Channel::Luminance,
            Some(s) => Channel::parse(s).ok_or_else(|| FactoryError::BadField {
                field: "channel".into(),
                msg: format!("expected one of r/g/b/a/luminance, got `{s}`"),
            })?,
        };
        Ok(BuiltNode {
            node: Box::new(PickChannelNode { channel }),
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
                "channel": {
                    "type": "string",
                    "enum": ["r", "g", "b", "a", "luminance"],
                    "default": "luminance",
                },
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(PickChannelFactory);
