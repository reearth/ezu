//! `line` — `Features + Brush -> Raster`. Wraps
//! [`paint_lines`](crate::paint_lines): hokusai brush stroke along
//! polylines with world-seeded pressure jitter.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError,
    FactoryCtx, FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use hokusai::Brush;
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::common::{
    canvas_into_raster, core_tile, downcast_brush, downcast_features, empty_raster, make_canvas,
    read_color, read_number, read_number_or, srgb_to_linear_rgba,
};
use crate::{paint_lines, LineStrokeStyle};

struct LineNode {
    color: [f32; 3],
    pressure_base: f32,
    pressure_jitter: f32,
    dtime: f32,
    radius_px: Option<f32>,
    opacity: Option<f32>,
}

impl Node for LineNode {
    fn op_name(&self) -> &'static str {
        "line"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[
            PortSpec {
                name: "features",
                kind: PortKind::Features,
                optional: false,
            },
            PortSpec {
                name: "brush",
                kind: PortKind::Brush,
                optional: false,
            },
        ];
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
        let feats = downcast_features(
            inputs[0]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("features".into()))?,
        )?;
        let brush_arc = downcast_brush(
            inputs[1]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("brush".into()))?,
        )?;
        if feats.lines.is_empty() {
            return Ok(empty_raster(ctx));
        }
        let mut canvas = make_canvas(ctx);
        // Clone brush and apply optional radius / opacity overrides.
        let mut brush: Brush = (*brush_arc).clone();
        if let Some(r) = self.radius_px {
            brush.get_mut(hokusai::BrushSetting::Radius).base_value = r.max(0.05).ln();
        }
        if let Some(o) = self.opacity {
            brush.get_mut(hokusai::BrushSetting::Opaque).base_value = o.clamp(0.0, 1.0);
        }
        let style = LineStrokeStyle {
            color: self.color,
            pressure_base: self.pressure_base,
            pressure_jitter: self.pressure_jitter,
            dtime: self.dtime,
        };
        paint_lines(&mut canvas, &feats.lines, feats.extent, core_tile(ctx), &brush, &style);
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"line");
        for c in self.color {
            h.update(&c.to_le_bytes());
        }
        for v in [self.pressure_base, self.pressure_jitter, self.dtime] {
            h.update(&v.to_le_bytes());
        }
        if let Some(r) = self.radius_px {
            h.update(&[1]);
            h.update(&r.to_le_bytes());
        } else {
            h.update(&[0]);
        }
        if let Some(o) = self.opacity {
            h.update(&[1]);
            h.update(&o.to_le_bytes());
        } else {
            h.update(&[0]);
        }
    }
}

pub(super) struct LineFactory;
impl NodeFactory for LineFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let brush = take_input_ref(fields, "brush")?;
        let color_srgb = read_color(fields, "color", ctx)?;
        let lin = srgb_to_linear_rgba(color_srgb);
        let color = [lin[0], lin[1], lin[2]];
        let pressure_base = read_number_or(fields, "pressure-base", ctx, 0.7)? as f32;
        let pressure_jitter = read_number_or(fields, "pressure-jitter", ctx, 0.2)? as f32;
        let dtime = read_number_or(fields, "dtime", ctx, 0.02)? as f32;
        let radius_px = if fields.contains_key("radius-px") {
            Some(read_number(fields, "radius-px", ctx)? as f32)
        } else {
            None
        };
        let opacity = if fields.contains_key("opacity") {
            Some(read_number(fields, "opacity", ctx)? as f32)
        } else {
            None
        };
        Ok(BuiltNode {
            node: Box::new(LineNode {
                color,
                pressure_base,
                pressure_jitter,
                dtime,
                radius_px,
                opacity,
            }),
            connections: vec![
                Connection {
                    port: "features".into(),
                    src: features,
                },
                Connection {
                    port: "brush".into(),
                    src: brush,
                },
            ],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Brush stroke along MVT polylines.",
            "properties": {
                "features": schema_frag::node_ref(),
                "brush": schema_frag::node_ref(),
                "color": schema_frag::color(),
                "radius-px": schema_frag::px_number(),
                "opacity": schema_frag::unit_number(),
                "pressure-base": schema_frag::unit_number(),
                "pressure-jitter": schema_frag::unit_number(),
                "dtime": { "type": "number", "minimum": 0.0 },
            },
            "required": ["features", "brush", "color"],
        })
    }
}
