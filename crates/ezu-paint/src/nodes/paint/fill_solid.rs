//! `fill-solid` — `Features -> Raster`. Wraps
//! [`paint_polygons`](crate::paint_polygons): solid fill, optional
//! outline, optional Gaussian blur.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    canvas_into_raster, downcast_features, empty_raster, make_canvas, read_color_u8,
    read_number_or, rgba8_to_color, tint_alpha_color,
};
use crate::{paint_polygons, WatercolorStyle};

struct FillSolidNode {
    fill: [u8; 4],
    fill_alpha: f32,
    edge: Option<[u8; 4]>,
    edge_width: f32,
    blur_sigma: f32,
}

impl Node for FillSolidNode {
    fn op_name(&self) -> &'static str {
        "fill-solid"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "features",
            accepts: &[PortKind::Features],
            optional: false,
        }];
        SPECS
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + (3.0 * self.blur_sigma.max(0.0)).ceil() as u32
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
        let style = WatercolorStyle {
            fill: tint_alpha_color(self.fill, self.fill_alpha),
            edge: self.edge.map(rgba8_to_color),
            edge_width: self.edge_width,
            blur_sigma: self.blur_sigma,
        };
        paint_polygons(&mut canvas, &feats.polygons, feats.extent, &style);
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"fill-solid");
        h.update(&self.fill);
        h.update(&self.fill_alpha.to_le_bytes());
        if let Some(e) = self.edge {
            h.update(&[1]);
            h.update(&e);
        } else {
            h.update(&[0]);
        }
        h.update(&self.edge_width.to_le_bytes());
        h.update(&self.blur_sigma.to_le_bytes());
    }
}

pub(super) struct FillSolidFactory;
impl NodeFactory for FillSolidFactory {
    fn op_name(&self) -> &'static str {
        "fill-solid"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let fill = read_color_u8(fields, "fill", ctx)?;
        let fill_alpha = read_number_or(fields, "fill-alpha", ctx, 1.0)? as f32;
        let edge = match fields.get("edge") {
            Some(_) => Some(read_color_u8(fields, "edge", ctx)?),
            None => None,
        };
        let edge_width = read_number_or(fields, "edge-width", ctx, 1.0)? as f32;
        let blur_sigma = read_number_or(fields, "blur-sigma", ctx, 0.0)? as f32;
        Ok(BuiltNode {
            node: Box::new(FillSolidNode {
                fill,
                fill_alpha,
                edge,
                edge_width,
                blur_sigma,
            }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Solid polygon fill with optional outline and Gaussian blur.",
            "properties": {
                "features": schema_frag::node_ref(),
                "fill": schema_frag::color(),
                "fill-alpha": schema_frag::unit_number(),
                "edge": schema_frag::color(),
                "edge-width": schema_frag::px_number(),
                "blur-sigma": schema_frag::px_number(),
            },
            "required": ["features", "fill"],
        })
    }
}

ezu_graph::submit_node!(FillSolidFactory);
