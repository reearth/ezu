//! `gradient-linear` — Linear gradient between two points. Coordinates
//! are fractions: `[0, 0]` = top-left, `[1, 1]` = bottom-right of the
//! tile (tile-anchored), of the full Mercator world at z=0
//! (world-anchored), or of the sprite (`kind: sprite`).

use ezu_graph::{
    BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::color_interp::InterpSpace;
use crate::nodes::common::{read_anchor, read_space, read_stops, read_xy, sample_stops, Anchor};
use crate::nodes::raster::generator_kind::{parse_generator_kind, GeneratorKind};
use crate::nodes::raster::gradient_common::render_gradient;

struct GradientLinearNode {
    start: [f32; 2],
    end: [f32; 2],
    stops: Vec<(f32, [f32; 4])>,
    space: InterpSpace,
    anchor: Anchor,
    out_kind: GeneratorKind,
}

impl Node for GradientLinearNode {
    fn op_name(&self) -> &'static str {
        "gradient-linear"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        match self.out_kind {
            GeneratorKind::Raster => PortKind::Raster,
            GeneratorKind::Sprite { .. } => PortKind::Sprite,
        }
    }
    fn coord_space(&self) -> CoordSpace {
        match (self.out_kind, self.anchor) {
            // Sprite output isn't tied to canvas geometry; coord space
            // is effectively local.
            (GeneratorKind::Sprite { .. }, _) => CoordSpace::Tile,
            (_, Anchor::Tile) => CoordSpace::Tile,
            (_, Anchor::World) => CoordSpace::World,
        }
    }
    fn eval(&self, ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        let dx = self.end[0] - self.start[0];
        let dy = self.end[1] - self.start[1];
        let len2 = dx * dx + dy * dy;
        let start = self.start;
        let stops = &self.stops;
        let space = self.space;
        let sample = |ux: f32, uy: f32| -> [f32; 4] {
            if len2 < 1e-12 {
                return stops.first().map(|s| s.1).unwrap_or([0.0; 4]);
            }
            let t = ((ux - start[0]) * dx + (uy - start[1]) * dy) / len2;
            sample_stops(stops, t, space)
        };
        Ok(render_gradient(ctx, self.out_kind, self.anchor, sample))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"gradient-linear");
        h.update(&self.start[0].to_le_bytes());
        h.update(&self.start[1].to_le_bytes());
        h.update(&self.end[0].to_le_bytes());
        h.update(&self.end[1].to_le_bytes());
        h.update(&[self.anchor as u8]);
        h.update(&[self.space.hash_tag()]);
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
}

pub(super) struct GradientLinearFactory;
impl NodeFactory for GradientLinearFactory {
    fn op_name(&self) -> &'static str {
        "gradient-linear"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let start = read_xy(fields, "start", ctx, [0.0, 0.0])?;
        let end = read_xy(fields, "end", ctx, [1.0, 0.0])?;
        let stops = read_stops(fields, "stops", ctx)?;
        let space = read_space(fields)?;
        let anchor = read_anchor(fields, "anchor", ctx)?;
        let out_kind = parse_generator_kind(fields, ctx)?;
        Ok(BuiltNode {
            node: Box::new(GradientLinearNode {
                start,
                end,
                stops,
                space,
                anchor,
                out_kind,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Linear gradient from `start` to `end` (fractional coords). `kind: raster` (default) samples in tile/world coords; `kind: sprite` samples in [0, 1] sprite coords at `width-px × height-px`.",
            "properties": {
                "start": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "default": [0.0, 0.0] },
                "end":   { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "default": [1.0, 0.0] },
                "stops": { "type": "array", "items": { "type": "array", "minItems": 2, "maxItems": 2 }, "minItems": 2 },
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

ezu_graph::submit_node!(GradientLinearFactory);
