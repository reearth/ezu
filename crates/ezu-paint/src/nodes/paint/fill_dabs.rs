//! `fill-dabs` — `Features -> Raster`. Wraps
//! [`paint_polygons_dabs`](crate::paint_polygons_dabs): watercolor
//! scatter-dab fill with world-deterministic jitter (seamless across
//! tiles).

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use hokusai::color::RgbaF32;
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    canvas_into_raster, core_tile, downcast_features, empty_raster, make_canvas, read_color,
    read_number, read_number_or, srgb_to_linear_rgba,
};
use crate::{paint_polygons_dabs, DabFillStyle};

struct FillDabsNode {
    color: [f32; 4],
    opacity: f32,
    radius_px: f32,
    hardness: f32,
    paint: f32,
    spacing_px: f32,
    position_jitter: f32,
    size_jitter: f32,
    opacity_jitter: f32,
    value_jitter: f32,
}

impl Node for FillDabsNode {
    fn op_name(&self) -> &'static str {
        "fill-dabs"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "features",
            kind: PortKind::Features,
            optional: false,
        }];
        SPECS
    }
    fn output(&self) -> PortKind {
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
        if feats.polygons.is_empty() {
            return Ok(empty_raster(ctx));
        }
        let mut canvas = make_canvas(ctx)?;
        let style = DabFillStyle {
            color: RgbaF32::new(self.color[0], self.color[1], self.color[2], 1.0),
            opacity: self.opacity,
            radius_px: self.radius_px,
            hardness: self.hardness,
            paint: self.paint,
            spacing_px: self.spacing_px,
            position_jitter: self.position_jitter,
            size_jitter: self.size_jitter,
            opacity_jitter: self.opacity_jitter,
            value_jitter: self.value_jitter,
        };
        paint_polygons_dabs(
            &mut canvas,
            &feats.polygons,
            feats.extent,
            core_tile(ctx),
            &style,
        );
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"fill-dabs");
        for c in self.color {
            h.update(&c.to_le_bytes());
        }
        for v in [
            self.opacity,
            self.radius_px,
            self.hardness,
            self.paint,
            self.spacing_px,
            self.position_jitter,
            self.size_jitter,
            self.opacity_jitter,
            self.value_jitter,
        ] {
            h.update(&v.to_le_bytes());
        }
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
        let color_srgb = read_color(fields, "color", ctx)?;
        let color = srgb_to_linear_rgba(color_srgb);
        let opacity = read_number(fields, "opacity", ctx)? as f32;
        let radius_px = read_number(fields, "radius-px", ctx)? as f32;
        let hardness = read_number_or(fields, "hardness", ctx, 0.5)? as f32;
        let paint = read_number_or(fields, "paint", ctx, 1.0)? as f32;
        let spacing_px = read_number(fields, "spacing-px", ctx)? as f32;
        let position_jitter = read_number_or(fields, "position-jitter", ctx, 0.9)? as f32;
        let size_jitter = read_number_or(fields, "size-jitter", ctx, 0.0)? as f32;
        let opacity_jitter = read_number_or(fields, "opacity-jitter", ctx, 0.0)? as f32;
        let value_jitter = read_number_or(fields, "value-jitter", ctx, 0.0)? as f32;
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
            }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
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
