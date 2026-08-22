//! `gradient-diamond` — Manhattan-distance gradient from `center`.
//! Useful for retro / pixel-art halos and square decorations.

use ezu_graph::{
    schema_frag, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, In, InReader,
    Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::color_interp::InterpSpace;
use crate::nodes::common::{
    hash_stops, read_anchor, read_space, read_stops, read_xy, resolve_stops, sample_stops, Anchor,
    StopsIn,
};
use crate::nodes::raster::generator_kind::{parse_generator_kind, GeneratorKind};
use crate::nodes::raster::gradient_common::render_gradient;

struct GradientDiamondNode {
    center: [f32; 2],
    radius: In<f64>,
    stops: StopsIn,
    space: InterpSpace,
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
        // Resolved here, so a `$param` stop costs one lookup per eval
        // rather than one per pixel.
        let stops = &resolve_stops(&self.stops, ctx, inputs)?;
        let space = self.space;
        let sample = |ux: f32, uy: f32| -> [f32; 4] {
            let t = ((ux - center[0]).abs() + (uy - center[1]).abs()) / r;
            sample_stops(stops, t, space)
        };
        Ok(render_gradient(ctx, self.out_kind, self.anchor, sample))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"gradient-diamond");
        h.update(&self.center[0].to_le_bytes());
        h.update(&self.center[1].to_le_bytes());
        self.radius.param_hash(h);
        h.update(&[self.anchor as u8]);
        h.update(&[self.space.hash_tag()]);
        hash_stops(&self.stops, h);
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
        let space = read_space(fields)?;
        let anchor = read_anchor(fields, "anchor", ctx)?;
        let mut r = InReader::new(fields, ctx, 0);
        let stops = read_stops(fields, "stops", &mut r)?;
        let radius = r.number_or("radius", 0.5)?;
        let parts = r.finish();
        let out_kind = parse_generator_kind(fields, ctx)?;
        Ok(BuiltNode {
            node: Box::new(GradientDiamondNode {
                center,
                radius,
                stops,
                space,
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
                "stops": { "type": "array", "minItems": 2,
                           "items": { "type": "array", "minItems": 2, "maxItems": 2,
                                      "items": [schema_frag::nested_number(serde_json::json!({ "type": "number" })), schema_frag::nested_color()] },
                           "description": "`[[t, color], ...]`. Either half of a pair may be a `$param`; the table is sorted by `t` on every eval, so stops need not be declared in order." },
                "anchor": { "type": "string", "enum": ["tile", "world"], "default": "tile" },
                "space": { "type": "string", "enum": ["rgb", "hsl", "hsv", "hcl", "lab"], "default": "rgb", "description": "Colour space the stops interpolate in; hue-based spaces take the shortest path." },
                "kind": { "type": "string", "enum": ["raster", "sprite"], "default": "raster" },
                "width-px": { "type": "integer", "minimum": 1 },
                "height-px": { "type": "integer", "minimum": 1 },
            },
            "required": ["stops"],
        })
    }
}

ezu_graph::submit_node!(GradientDiamondFactory);
