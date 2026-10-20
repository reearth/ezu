//! `gradient-diamond` — Manhattan-distance gradient from `center`.
//! Useful for retro / pixel-art halos and square decorations.

use ezu_graph::{
    schema_frag, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, In, InEnum,
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

struct GradientDiamondNode {
    center: [f32; 2],
    radius: In<f64>,
    stops: StopsIn,
    space: InEnum<InterpSpace>,
    interpolation: InEnum<StopInterpolation>,
    anchor: InEnum<Anchor>,
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
        self.out_kind.port_kind()
    }
    fn coord_space(&self) -> CoordSpace {
        // Descriptive only, so the declared anchor stands for whatever a
        // `$param` sets it to at render time.
        match (self.out_kind, self.anchor.fallback()) {
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
        let space = self.space.get(ctx)?;
        let anchor = self.anchor.get(ctx)?;
        let stops = &resolve_stops(&self.stops, space, ctx, inputs)?;
        let position =
            |ux: f32, uy: f32| -> f32 { ((ux - center[0]).abs() + (uy - center[1]).abs()) / r };
        let interpolation = self.interpolation.get(ctx)?;
        let color = |t: f32| color_at(stops, t, space, interpolation);
        Ok(render_gradient(ctx, self.out_kind, anchor, position, color))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"gradient-diamond");
        h.update(&self.center[0].to_le_bytes());
        h.update(&self.center[1].to_le_bytes());
        self.radius.param_hash(h);
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
        let out_kind = parse_generator_kind(fields, ctx)?;
        let mut r = InReader::new(fields, ctx, 0);
        let space = read_space(&mut r, fields)?;
        let interpolation = r.enum_or(
            "interpolation",
            StopInterpolation::Linear,
            STOP_INTERPOLATIONS,
        )?;
        let anchor = r.enum_or("anchor", Anchor::Tile, ANCHORS)?;
        let stops = read_gradient_stops(fields, ctx, &mut r, out_kind)?;
        let radius = r.number_or("radius", 0.5)?;
        let parts = r.finish();
        Ok(BuiltNode {
            node: Box::new(GradientDiamondNode {
                center,
                radius,
                stops,
                space,
                interpolation,
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
            "description": "Diamond gradient from `center`. Gradient parameter is Manhattan (|dx| + |dy|) distance / radius. `kind: sprite` switches to sprite-local [0, 1] coords; `kind: scalar` emits a canvas-sized ScalarField of that parameter instead of a colour, clamped to [0, 1], and takes no `stops`.",
            "properties": {
                "center": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "default": [0.5, 0.5] },
                "radius": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "default": 0.5 })),
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

ezu_graph::submit_node!(GradientDiamondFactory);
