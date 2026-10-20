//! `gradient-linear` — Linear gradient between two points. Coordinates
//! are fractions: `[0, 0]` = top-left, `[1, 1]` = bottom-right of the
//! tile (tile-anchored), of the full Mercator world at z=0
//! (world-anchored), or of the sprite (`kind: sprite`).

use ezu_graph::{
    schema_frag, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, InEnum,
    InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::color_interp::InterpSpace;
use crate::nodes::common::{
    anchor_schema, color_at, hash_stops, interpolation_schema, read_space, read_xy, resolve_stops,
    space_schema, Anchor, StopInterpolation, StopsIn, ANCHORS, STOP_INTERPOLATIONS,
};
use crate::nodes::raster::generator_kind::{
    generator_kind_schema, parse_generator_kind, GeneratorKind,
};
use crate::nodes::raster::gradient_common::{read_gradient_stops, render_gradient, stops_required};

struct GradientLinearNode {
    start: [f32; 2],
    end: [f32; 2],
    stops: StopsIn,
    space: InEnum<InterpSpace>,
    interpolation: InEnum<StopInterpolation>,
    anchor: InEnum<Anchor>,
    out_kind: GeneratorKind,
    param_refs: Vec<String>,
}

impl Node for GradientLinearNode {
    fn op_name(&self) -> &'static str {
        "gradient-linear"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        self.out_kind.port_kind()
    }
    fn coord_space(&self) -> CoordSpace {
        // Descriptive only, so the declared anchor stands for whatever a
        // `$param` sets it to at render time.
        match (self.out_kind, self.anchor.fallback()) {
            // Sprite output isn't tied to canvas geometry; coord space
            // is effectively local.
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
        let dx = self.end[0] - self.start[0];
        let dy = self.end[1] - self.start[1];
        let len2 = dx * dx + dy * dy;
        let start = self.start;
        // Resolved here, so a `$param` stop costs one lookup per eval
        // rather than one per pixel.
        let space = self.space.get(ctx)?;
        let anchor = self.anchor.get(ctx)?;
        let stops = &resolve_stops(&self.stops, space, ctx, inputs)?;
        // A zero-length gradient sits before its first stop everywhere.
        let position = |ux: f32, uy: f32| -> f32 {
            if len2 < 1e-12 {
                return f32::NEG_INFINITY;
            }
            ((ux - start[0]) * dx + (uy - start[1]) * dy) / len2
        };
        let interpolation = self.interpolation.get(ctx)?;
        let color = |t: f32| color_at(stops, t, space, interpolation);
        Ok(render_gradient(ctx, self.out_kind, anchor, position, color))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"gradient-linear");
        h.update(&self.start[0].to_le_bytes());
        h.update(&self.start[1].to_le_bytes());
        h.update(&self.end[0].to_le_bytes());
        h.update(&self.end[1].to_le_bytes());
        self.anchor.param_hash(h);
        self.space.param_hash(h);
        self.interpolation.param_hash(h);
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
        let out_kind = parse_generator_kind(fields, ctx)?;
        let mut r = InReader::new(fields, ctx, 0);
        let stops = read_gradient_stops(fields, ctx, &mut r, out_kind)?;
        let space = read_space(&mut r, fields)?;
        let interpolation = r.enum_or(
            "interpolation",
            StopInterpolation::Linear,
            STOP_INTERPOLATIONS,
        )?;
        let anchor = r.enum_or("anchor", Anchor::Tile, ANCHORS)?;
        Ok(BuiltNode {
            node: Box::new(GradientLinearNode {
                start,
                end,
                stops,
                space,
                interpolation,
                anchor,
                out_kind,
                param_refs: r.finish().param_refs,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Linear gradient from `start` to `end` (fractional coords). `kind: raster` (default) samples in tile/world coords; `kind: sprite` samples in [0, 1] sprite coords at `width-px × height-px`; `kind: scalar` emits a canvas-sized ScalarField of the position along the gradient — the projection onto `start → end`, 0 at `start` and 1 at `end`, clamped to [0, 1] — and takes no `stops`.",
            "properties": {
                "start": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "default": [0.0, 0.0] },
                "end":   { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "default": [1.0, 0.0] },
                "stops": { "type": "array", "minItems": 2,
                           "items": { "type": "array", "minItems": 2, "maxItems": 2,
                                      "items": [schema_frag::nested_number(serde_json::json!({ "type": "number" })), schema_frag::nested_color()] },
                           "description": "`[[t, color], ...]`. Either half of a pair may be a `$param`; the table is sorted by `t` on every eval, so stops need not be declared in order." },
                "anchor": anchor_schema("tile", None),
                "space": space_schema("Colour space the stops interpolate in; hue-based spaces take the shortest path."),
                "interpolation": interpolation_schema(),
                "kind": generator_kind_schema(),
                "width-px": { "type": "integer", "minimum": 1 },
                "height-px": { "type": "integer", "minimum": 1 },
            },
            "anyOf": stops_required(),
        })
    }
}

ezu_graph::submit_node!(GradientLinearFactory);
