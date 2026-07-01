//! `mix` — `(Raster, Raster) -> Raster`. Per-pixel interpolate between two
//! inputs by a scalar `t` (0..1) in a selectable colour `space`
//! (`rgb`/`hsl`/`hsv`/`hcl`/`lab`). Unlike `blend` (which composites `over`
//! onto `base` with alpha), `mix` is a straight colour tween — ideal for
//! param-driven theming (day↔night palettes, seasonal tints) and for
//! blending two gradients perceptually.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::color_interp::{interpolate, InterpSpace};
use crate::nodes::common::{
    raster_or_sprite_output, read_space, unwrap_raster_or_sprite, wrap_raster_like,
    ACCEPTS_RASTER_OR_SPRITE,
};

struct MixNode {
    t: In<f64>,
    space: InterpSpace,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for MixNode {
    fn op_name(&self) -> &'static str {
        "mix"
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
        let a_in = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("a".into()))?;
        let (a, kind) = unwrap_raster_or_sprite(a_in, "a")?;
        let b_in = inputs[1]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("b".into()))?;
        let (b, _) = unwrap_raster_or_sprite(b_in, "b")?;
        if a.width != b.width || a.height != b.height {
            return Err(EvalError::Other("mix: a/b size mismatch".into()));
        }
        let t = (self.t.get(ctx, inputs)? as f32).clamp(0.0, 1.0);
        let mut out = RasterBuf::new(a.width, a.height);
        for i in (0..a.pixels.len()).step_by(4) {
            let ca = demul(&a.pixels[i..i + 4]);
            let cb = demul(&b.pixels[i..i + 4]);
            let m = interpolate(ca, cb, t, self.space);
            let oa = m[3].clamp(0.0, 1.0);
            out.pixels[i] = to_u8(m[0] * oa);
            out.pixels[i + 1] = to_u8(m[1] * oa);
            out.pixels[i + 2] = to_u8(m[2] * oa);
            out.pixels[i + 3] = to_u8(oa);
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"mix");
        h.update(&[self.space.hash_tag()]);
        self.t.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct MixFactory;
impl NodeFactory for MixFactory {
    fn op_name(&self) -> &'static str {
        "mix"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let a = take_input_ref(fields, "a")?;
        let b = take_input_ref(fields, "b")?;
        let space = read_space(fields)?;
        // Scalar ports start after the two fixed raster inputs.
        let mut r = InReader::new(fields, ctx, 2);
        let t = r.number_or("t", 0.5)?;
        let parts = r.finish();

        let mut ports = vec![
            PortSpec {
                name: "a",
                accepts: ACCEPTS_RASTER_OR_SPRITE,
                optional: false,
            },
            PortSpec {
                name: "b",
                accepts: ACCEPTS_RASTER_OR_SPRITE,
                optional: false,
            },
        ];
        ports.extend(parts.ports);

        let mut connections = vec![
            Connection {
                port: "a".into(),
                src: a,
            },
            Connection {
                port: "b".into(),
                src: b,
            },
        ];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(MixNode {
                t,
                space,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Interpolate two rasters per-pixel by scalar `t` (0..1) in colour `space`. A straight colour tween (contrast with `blend`, which composites with alpha). `t` accepts a literal, `$param`, or `@node` scalar for param-driven theming.",
            "properties": {
                "a": schema_frag::node_ref(),
                "b": schema_frag::node_ref(),
                "t": schema_frag::unit_number(),
                "space": { "type": "string", "enum": ["rgb", "hsl", "hsv", "hcl", "lab"], "default": "rgb", "description": "Colour space the interpolation runs in; hue-based spaces take the shortest path." },
            },
            "required": ["a", "b"],
        })
    }
}

#[inline]
fn demul(px: &[u8]) -> [f32; 4] {
    let a = px[3] as f32 / 255.0;
    if a <= 0.0 {
        return [0.0, 0.0, 0.0, 0.0];
    }
    [
        (px[0] as f32 / 255.0 / a).min(1.0),
        (px[1] as f32 / 255.0 / a).min(1.0),
        (px[2] as f32 / 255.0 / a).min(1.0),
        a,
    ]
}

#[inline]
fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

ezu_graph::submit_node!(MixFactory);
