//! `fill-dabs` — `Features -> Raster`. Wraps
//! [`paint_polygons_dabs`](crate::paint_polygons_dabs): watercolor
//! scatter-dab fill with world-deterministic jitter (seamless across
//! tiles).

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use hokusai::color::RgbaF32;
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    canvas_into_raster, core_tile, downcast_features, empty_raster, make_canvas,
    srgb_to_linear_rgba,
};
use crate::{paint_polygons_dabs, DabFillStyle};

struct FillDabsNode {
    color: In<[f32; 4]>,
    opacity: In<f64>,
    radius_px: In<f64>,
    hardness: In<f64>,
    paint: In<f64>,
    spacing_px: In<f64>,
    position_jitter: In<f64>,
    size_jitter: In<f64>,
    opacity_jitter: In<f64>,
    value_jitter: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for FillDabsNode {
    fn op_name(&self) -> &'static str {
        "fill-dabs"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::World
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let feats = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("features".into()))?;
        let feats = downcast_features(feats)?;
        if !feats.has_polygons() {
            return Ok(empty_raster(ctx));
        }
        let color = srgb_to_linear_rgba(self.color.get(ctx, inputs)?);
        let mut canvas = make_canvas(ctx)?;
        let style = DabFillStyle {
            color: RgbaF32::new(color[0], color[1], color[2], 1.0),
            opacity: self.opacity.get(ctx, inputs)? as f32,
            radius_px: self.radius_px.get(ctx, inputs)? as f32,
            hardness: self.hardness.get(ctx, inputs)? as f32,
            paint: self.paint.get(ctx, inputs)? as f32,
            spacing_px: self.spacing_px.get(ctx, inputs)? as f32,
            position_jitter: self.position_jitter.get(ctx, inputs)? as f32,
            size_jitter: self.size_jitter.get(ctx, inputs)? as f32,
            opacity_jitter: self.opacity_jitter.get(ctx, inputs)? as f32,
            value_jitter: self.value_jitter.get(ctx, inputs)? as f32,
        };
        let polygons: Vec<_> = feats.polygons().cloned().collect();
        paint_polygons_dabs(&mut canvas, &polygons, feats.extent, core_tile(ctx), &style);
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"fill-dabs");
        self.color.param_hash(h);
        self.opacity.param_hash(h);
        self.radius_px.param_hash(h);
        self.hardness.param_hash(h);
        self.paint.param_hash(h);
        self.spacing_px.param_hash(h);
        self.position_jitter.param_hash(h);
        self.size_jitter.param_hash(h);
        self.opacity_jitter.param_hash(h);
        self.value_jitter.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct FillDabsFactory;
impl NodeFactory for FillDabsFactory {
    fn op_name(&self) -> &'static str {
        "fill-dabs"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let mut r = InReader::new(fields, ctx, 1);
        let color = r.color("color")?;
        let opacity = r.number("opacity")?;
        let radius_px = r.number("radius-px")?;
        let hardness = r.number_or("hardness", 0.5)?;
        let paint = r.number_or("paint", 1.0)?;
        let spacing_px = r.number("spacing-px")?;
        let position_jitter = r.number_or("position-jitter", 0.9)?;
        let size_jitter = r.number_or("size-jitter", 0.0)?;
        let opacity_jitter = r.number_or("opacity-jitter", 0.0)?;
        let value_jitter = r.number_or("value-jitter", 0.0)?;
        let parts = r.finish();

        let mut ports = vec![PortSpec {
            name: "features",
            accepts: &[PortKind::Features],
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "features".into(),
            src: features,
        }];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(FillDabsNode {
                color,
                opacity,
                radius_px,
                hardness,
                paint,
                spacing_px,
                position_jitter,
                size_jitter,
                opacity_jitter,
                value_jitter,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Watercolor scatter-dab fill with world-deterministic jitter (seamless across tiles).",
            "properties": {
                "features": schema_frag::node_ref(),
                "color": schema_frag::color(),
                "opacity": schema_frag::unit_number(),
                "radius-px": schema_frag::px_number(),
                "hardness": schema_frag::unit_number(),
                "paint": schema_frag::unit_number(),
                "spacing-px": schema_frag::px_number(),
                "position-jitter": schema_frag::unit_number(),
                "size-jitter": schema_frag::unit_number(),
                "opacity-jitter": schema_frag::unit_number(),
                "value-jitter": schema_frag::unit_number(),
            },
            "required": ["features", "color", "opacity", "radius-px", "spacing-px"],
        })
    }
}

ezu_graph::submit_node!(FillDabsFactory);
