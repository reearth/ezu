//! `erode` / `dilate` — morphological min / max box filter over a
//! `Raster|Sprite` (pass-through). `radius-px` controls the half-size
//! of the square neighbourhood; the op grows the upstream pad by the
//! same amount so the filter stays seamless at tile borders.
//!
//! Operates per-channel on premultiplied RGBA8. The classic use is
//! cleaning up a mask after `color-to-alpha`: `erode` shrinks the
//! covered region, `dilate` grows it. For a circular kernel, run the
//! op twice with smaller radii — the separable box is fast and good
//! enough for most map stylization needs.

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

#[derive(Debug, Clone, Copy)]
enum Op {
    Erode,
    Dilate,
}

impl Op {
    fn tag(self) -> &'static [u8] {
        match self {
            Op::Erode => b"erode",
            Op::Dilate => b"dilate",
        }
    }
    fn combine(self, a: u8, b: u8) -> u8 {
        match self {
            Op::Erode => a.min(b),
            Op::Dilate => a.max(b),
        }
    }
    fn ident(self) -> u8 {
        match self {
            Op::Erode => 255,
            Op::Dilate => 0,
        }
    }
}

struct MorphNode {
    op: Op,
    radius: In<f64>,
    /// Build-time upper bound on `radius-px`, for pad propagation.
    radius_bound: u32,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for MorphNode {
    fn op_name(&self) -> &'static str {
        match self.op {
            Op::Erode => "erode",
            Op::Dilate => "dilate",
        }
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        raster_or_sprite_output(input_kinds)
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + self.radius_bound
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
        let radius = (self.radius.get(ctx, inputs)?.round().clamp(0.0, 256.0)) as u32;
        if radius == 0 {
            return Ok(wrap_raster_like(src, kind));
        }
        let w = src.width;
        let h = src.height;
        // Separable: horizontal pass then vertical pass.
        let mid = run_axis(&src.pixels, w, h, radius, self.op, Axis::Horizontal);
        let final_ = run_axis(&mid, w, h, radius, self.op, Axis::Vertical);
        Ok(wrap_raster_like(
            Arc::new(RasterBuf {
                width: w,
                height: h,
                pixels: final_,
            }),
            kind,
        ))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(self.op.tag());
        self.radius.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

#[derive(Debug, Clone, Copy)]
enum Axis {
    Horizontal,
    Vertical,
}

fn run_axis(src: &[u8], w: u32, h: u32, radius: u32, op: Op, axis: Axis) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    let r = radius as i32;
    let (outer, inner) = match axis {
        Axis::Horizontal => (h, w),
        Axis::Vertical => (w, h),
    };
    for o in 0..outer {
        for i in 0..inner {
            let mut acc = [op.ident(); 4];
            for k in -r..=r {
                let ii = i as i32 + k;
                if ii < 0 || ii >= inner as i32 {
                    continue;
                }
                let (x, y) = match axis {
                    Axis::Horizontal => (ii as u32, o),
                    Axis::Vertical => (o, ii as u32),
                };
                let off = ((y * w + x) * 4) as usize;
                for c in 0..4 {
                    acc[c] = op.combine(acc[c], src[off + c]);
                }
            }
            let dst = match axis {
                Axis::Horizontal => ((o * w + i) * 4) as usize,
                Axis::Vertical => ((i * w + o) * 4) as usize,
            };
            out[dst..dst + 4].copy_from_slice(&acc);
        }
    }
    out
}

pub(super) struct ErodeFactory;
impl NodeFactory for ErodeFactory {
    fn op_name(&self) -> &'static str {
        "erode"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        build_morph(fields, ctx, Op::Erode)
    }
    fn schema(&self) -> Value {
        morph_schema("Per-channel morphological min over a square kernel. Shrinks bright / opaque regions; classic mask cleanup after `color-to-alpha`. Separable box implementation; grows upstream pad by `radius-px`.")
    }
}

pub(super) struct DilateFactory;
impl NodeFactory for DilateFactory {
    fn op_name(&self) -> &'static str {
        "dilate"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        build_morph(fields, ctx, Op::Dilate)
    }
    fn schema(&self) -> Value {
        morph_schema("Per-channel morphological max over a square kernel. Grows bright / opaque regions; pair with `erode` to clean up speckle noise (open / close). Separable box implementation; grows upstream pad by `radius-px`.")
    }
}

fn build_morph(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
    op: Op,
) -> Result<BuiltNode, FactoryError> {
    let input = take_input_ref(fields, "input")?;
    let mut r = InReader::new(fields, ctx, 1);
    let radius = r.number("radius-px")?;
    let parts = r.finish();
    let radius_bound = radius
        .static_bound()
        .ok_or_else(|| FactoryError::BadField {
            field: "radius-px".into(),
            msg: "pad depends on radius-px at build time: use a literal, or a `$param` with \
                  `max` (a `@node` port has no static bound)"
                .into(),
        })?
        .round();
    if !(0.0..=256.0).contains(&radius_bound) {
        return Err(FactoryError::BadField {
            field: "radius-px".into(),
            msg: "expected a non-negative integer ≤ 256".into(),
        });
    }

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
        node: Box::new(MorphNode {
            op,
            radius,
            radius_bound: radius_bound as u32,
            ports,
            param_refs: parts.param_refs,
        }),
        connections,
    })
}

fn morph_schema(description: &str) -> Value {
    serde_json::json!({
        "description": description,
        "properties": {
            "input": schema_frag::node_ref(),
            "radius-px": schema_frag::in_number(serde_json::json!({
                "type": "integer", "minimum": 0, "maximum": 256
            })),
        },
        "required": ["input", "radius-px"],
    })
}

ezu_graph::submit_node!(ErodeFactory);
ezu_graph::submit_node!(DilateFactory);
