//! `fill-solid` — `Features -> Raster`. Wraps
//! [`paint_polygons`](crate::paint_polygons): solid fill, optional
//! outline, optional Gaussian blur.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    canvas_into_raster, color_f32_to_u8, downcast_features, empty_raster, make_canvas,
    rgba8_to_color, tint_alpha_color,
};
use crate::{paint_polygons, WatercolorStyle};

struct FillSolidNode {
    fill: In<[f32; 4]>,
    fill_alpha: In<f64>,
    edge: Option<In<[f32; 4]>>,
    edge_width: In<f64>,
    blur_sigma: In<f64>,
    /// Build-time upper bound on `blur-sigma`, for pad propagation.
    blur_sigma_bound: f32,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for FillSolidNode {
    fn op_name(&self) -> &'static str {
        "fill-solid"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + (3.0 * self.blur_sigma_bound.max(0.0)).ceil() as u32
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
        let fill = color_f32_to_u8(self.fill.get(ctx, inputs)?);
        let fill_alpha = self.fill_alpha.get(ctx, inputs)? as f32;
        let edge = match &self.edge {
            Some(e) => Some(color_f32_to_u8(e.get(ctx, inputs)?)),
            None => None,
        };
        let mut canvas = make_canvas(ctx)?;
        let style = WatercolorStyle {
            fill: tint_alpha_color(fill, fill_alpha),
            edge: edge.map(rgba8_to_color),
            edge_width: self.edge_width.get(ctx, inputs)? as f32,
            blur_sigma: self.blur_sigma.get(ctx, inputs)? as f32,
        };
        paint_polygons(&mut canvas, &feats.polygons, feats.extent, &style);
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"fill-solid");
        self.fill.param_hash(h);
        self.fill_alpha.param_hash(h);
        if let Some(e) = &self.edge {
            h.update(&[1]);
            e.param_hash(h);
        } else {
            h.update(&[0]);
        }
        self.edge_width.param_hash(h);
        self.blur_sigma.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        let mut r = InReader::new(fields, ctx, 1);
        let fill = r.color("fill")?;
        let fill_alpha = r.number_or("fill-alpha", 1.0)?;
        let edge = r.color_opt("edge")?;
        let edge_width = r.number_or("edge-width", 1.0)?;
        let blur_sigma = r.number_or("blur-sigma", 0.0)?;
        let parts = r.finish();
        let blur_sigma_bound = blur_sigma
            .static_bound()
            .ok_or_else(|| FactoryError::BadField {
                field: "blur-sigma".into(),
                msg: "pad depends on blur-sigma at build time: use a literal, or a `$param` \
                          with `max` (a `@node` port has no static bound)"
                    .into(),
            })? as f32;

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
            node: Box::new(FillSolidNode {
                fill,
                fill_alpha,
                edge,
                edge_width,
                blur_sigma,
                blur_sigma_bound,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
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
