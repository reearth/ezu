//! `gradient-conic` — Sweep gradient around `center`. Gradient
//! parameter is the angle (clockwise from `start-angle`) normalized
//! to `[0, 1)`. `start-angle` is in degrees.

use ezu_graph::{
    schema_frag, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, In, InReader,
    Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{read_anchor, read_stops, read_xy, sample_stops, Anchor};
use crate::nodes::raster::generator_kind::{parse_generator_kind, GeneratorKind};
use crate::nodes::raster::gradient_common::render_gradient;

struct GradientConicNode {
    center: [f32; 2],
    start_angle: In<f64>, // degrees
    stops: Vec<(f32, [f32; 4])>,
    anchor: Anchor,
    out_kind: GeneratorKind,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for GradientConicNode {
    fn op_name(&self) -> &'static str {
        "gradient-conic"
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
        let start_rad = (self.start_angle.get(ctx, inputs)? as f32).to_radians();
        let center = self.center;
        let stops = &self.stops;
        let sample = |ux: f32, uy: f32| -> [f32; 4] {
            let dx = ux - center[0];
            let dy = uy - center[1];
            let ang = dy.atan2(dx) - start_rad;
            let t = ang.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
            sample_stops(stops, t)
        };
        Ok(render_gradient(ctx, self.out_kind, self.anchor, sample))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"gradient-conic");
        h.update(&self.center[0].to_le_bytes());
        h.update(&self.center[1].to_le_bytes());
        self.start_angle.param_hash(h);
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

pub(super) struct GradientConicFactory;
impl NodeFactory for GradientConicFactory {
    fn op_name(&self) -> &'static str {
        "gradient-conic"
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
        let start_angle = r.number_or("start-angle", 0.0)?;
        let parts = r.finish();
        let out_kind = parse_generator_kind(fields, ctx)?;
        Ok(BuiltNode {
            node: Box::new(GradientConicNode {
                center,
                start_angle,
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
            "description": "Conic (sweep) gradient around `center`. Gradient parameter is the clockwise angle from `start-angle` (degrees) normalized to [0, 1). 0° points along +x. `kind: sprite` switches to sprite-local [0, 1] coords.",
            "properties": {
                "center": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "default": [0.5, 0.5] },
                "start-angle": schema_frag::number(),
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

ezu_graph::submit_node!(GradientConicFactory);
