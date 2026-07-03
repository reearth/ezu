//! `transform` — `Features -> Features`. Apply an affine (translate +
//! rotate + scale) transform to every vertex. Rotation happens around
//! `pivot` (in feature-space coordinates, default = origin) before
//! the final translation. Scale is per-axis.

use ezu_features::ops::transform::transform;
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    downcast_features, features_value, read_number_or, read_xy, FeatureGroup,
};

struct TransformNode {
    translate: (f64, f64),
    rotation_rad: f64,
    scale: (f64, f64),
    pivot: (f64, f64),
}

impl Node for TransformNode {
    fn op_name(&self) -> &'static str {
        "transform"
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
        PortKind::Features
    }
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::Tile
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let feats = downcast_features(
            inputs[0]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("features".into()))?,
        )?;
        // Per group: apply the affine to each feature's vertices, carrying
        // properties.
        let mut out_groups = Vec::with_capacity(feats.groups.len());
        for g in &feats.groups {
            let mut points = g.points.clone();
            let mut lines = g.lines.clone();
            let mut polygons = g.polygons.clone();
            transform(
                &mut points,
                &mut lines,
                &mut polygons,
                self.scale,
                self.rotation_rad,
                self.pivot,
                self.translate,
            );
            out_groups.push(FeatureGroup {
                properties: g.properties.clone(),
                polygons,
                lines,
                points,
            });
        }
        Ok(features_value(feats.extent, out_groups))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"transform");
        h.update(&self.translate.0.to_le_bytes());
        h.update(&self.translate.1.to_le_bytes());
        h.update(&self.rotation_rad.to_le_bytes());
        h.update(&self.scale.0.to_le_bytes());
        h.update(&self.scale.1.to_le_bytes());
        h.update(&self.pivot.0.to_le_bytes());
        h.update(&self.pivot.1.to_le_bytes());
    }
}

pub(super) struct TransformFactory;
impl NodeFactory for TransformFactory {
    fn op_name(&self) -> &'static str {
        "transform"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let tx = read_xy(fields, "translate", ctx, [0.0, 0.0])?;
        let rotation_deg = read_number_or(fields, "rotation-deg", ctx, 0.0)?;
        let scale_uniform = read_number_or(fields, "scale", ctx, 1.0)?;
        let scale_xy = read_xy(
            fields,
            "scale-xy",
            ctx,
            [scale_uniform as f32, scale_uniform as f32],
        )?;
        let pivot = read_xy(fields, "pivot", ctx, [0.0, 0.0])?;
        Ok(BuiltNode {
            node: Box::new(TransformNode {
                translate: (tx[0] as f64, tx[1] as f64),
                rotation_rad: rotation_deg.to_radians(),
                scale: (scale_xy[0] as f64, scale_xy[1] as f64),
                pivot: (pivot[0] as f64, pivot[1] as f64),
            }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Translate / rotate / scale every input vertex. Rotation happens around `pivot` (defaults to the origin) before the final translation. `scale-xy` overrides the uniform `scale` when both are present.",
            "properties": {
                "features": schema_frag::node_ref(),
                "translate": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 },
                "rotation-deg": { "type": "number", "default": 0.0 },
                "scale": { "type": "number", "default": 1.0 },
                "scale-xy": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 },
                "pivot": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 },
            },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(TransformFactory);
