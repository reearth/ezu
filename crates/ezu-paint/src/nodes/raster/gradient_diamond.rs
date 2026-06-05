//! `gradient-diamond` — Manhattan-distance gradient from `center`.
//! Useful for retro / pixel-art halos and square decorations.

use ezu_graph::{
    schema_frag, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, In, InReader,
    Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{read_anchor, read_stops, read_xy, sample_stops, Anchor};
use crate::nodes::raster::generator_kind::{parse_generator_kind, GeneratorKind};
use crate::nodes::raster::gradient_common::render_gradient;

struct GradientDiamondNode {
    center: [f32; 2],
    radius: In<f64>,
    stops: Vec<(f32, [f32; 4])>,
    anchor: Anchor,
    out_kind: GeneratorKind,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for GradientDiamondNode {
    fn op_name(&self) -> &'static str {
        "gradient-diamond"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        match self.out_kind {
            GeneratorKind::Raster => PortKind::Raster,
            GeneratorKind::Sprite { .. } => PortKind::Sprite,
        }
    }
    fn coord_space(&self) -> CoordSpace {
        match (self.out_kind, self.anchor) {
            (GeneratorKind::Sprite { .. }, _) => CoordSpace::Tile,
            (_, Anchor::Tile) => CoordSpace::Tile,
            (_, Anchor::World) => CoordSpace::World,
        }
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let r = (self.radius.get(ctx, inputs)? as f32).max(1e-6);
        let center = self.center;
        let stops = &self.stops;
        let sample = |ux: f32, uy: f32| -> [f32; 4] {
            let t = ((ux - center[0]).abs() + (uy - center[1]).abs()) / r;
            sample_stops(stops, t)
        };
        Ok(render_gradient(ctx, self.out_kind, self.anchor, sample))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"gradient-diamond");
        h.update(&self.center[0].to_le_bytes());
        h.update(&self.center[1].to_le_bytes());
        self.radius.param_hash(h);
        h.update(&[self.anchor as u8]);
        for (t, c) in &self.stops {
            h.update(&t.to_le_bytes());
            for v in c {
                h.update(&v.to_le_bytes());
            }
        }
        let (tag, dims) = self.out_kind.hash_tag();
        h.update(&tag);
        if let Some((w, hh)) = dims {
            h.update(&w.to_le_bytes());
            h.update(&hh.to_le_bytes());
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct GradientDiamondFactory;
impl NodeFactory for GradientDiamondFactory {
    fn op_name(&self) -> &'static str {
        "gradient-diamond"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let center = read_xy(fields, "center", ctx, [0.5, 0.5])?;
        let stops = read_stops(fields, "stops", ctx)?;
        let anchor = read_anchor(fields, "anchor", ctx)?;
        let mut r = InReader::new(fields, ctx, 0);
        let radius = r.number_or("radius", 0.5)?;
        let parts = r.finish();
        let out_kind = parse_generator_kind(fields, ctx)?;
        Ok(BuiltNode {
            node: Box::new(GradientDiamondNode {
                center,
                radius,
                stops,
                anchor,
                out_kind,
                ports: parts.ports,
                param_refs: parts.param_refs,
            }),
            connections: parts.connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Diamond gradient from `center`. Gradient parameter is Manhattan (|dx| + |dy|) distance / radius. `kind: sprite` switches to sprite-local [0, 1] coords.",
            "properties": {
                "center": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "default": [0.5, 0.5] },
                "radius": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "default": 0.5 })),
                "stops": { "type": "array", "items": { "type": "array", "minItems": 2, "maxItems": 2 }, "minItems": 2 },
                "anchor": { "type": "string", "enum": ["tile", "world"], "default": "tile" },
                "kind": { "type": "string", "enum": ["raster", "sprite"], "default": "raster" },
                "width-px": { "type": "integer", "minimum": 1 },
                "height-px": { "type": "integer", "minimum": 1 },
            },
            "required": ["stops"],
        })
    }
}

ezu_graph::submit_node!(GradientDiamondFactory);
