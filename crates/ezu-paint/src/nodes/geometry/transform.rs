//! `transform` — `Features -> Features`. Apply an affine (translate +
//! rotate + scale) transform to every vertex. Rotation happens around
//! `pivot` (in feature-space coordinates, default = origin) before
//! the final translation. Scale is per-axis.
//!
//! The scalar forms — `translate-x` / `translate-y`, `rotation-deg`,
//! `scale` / `scale-x` / `scale-y` — are `In<f64>` fields, so they take a
//! literal, a `$param` a caller overrides per render, or an `@node`
//! scalar port. Nothing here decides canvas padding, so there is no
//! static-bound requirement. The `[x, y]` array forms (`translate`,
//! `scale-xy`, `pivot`) stay literal: an array is not a scalar, so a
//! param cannot stand in for one.

use ezu_features::ops::transform::transform;
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, read_xy, FeatureGroup};

struct TransformNode {
    translate_x: In<f64>,
    translate_y: In<f64>,
    rotation_deg: In<f64>,
    scale: In<f64>,
    scale_x: In<f64>,
    scale_y: In<f64>,
    pivot: (f64, f64),
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for TransformNode {
    fn op_name(&self) -> &'static str {
        "transform"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Features
    }
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::Tile
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
        let translate = (
            self.translate_x.get(ctx, inputs)?,
            self.translate_y.get(ctx, inputs)?,
        );
        let rotation_rad = self.rotation_deg.get(ctx, inputs)?.to_radians();
        let uniform = self.scale.get(ctx, inputs)?;
        let scale = (
            uniform * self.scale_x.get(ctx, inputs)?,
            uniform * self.scale_y.get(ctx, inputs)?,
        );
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
                scale,
                rotation_rad,
                self.pivot,
                translate,
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
        self.translate_x.param_hash(h);
        self.translate_y.param_hash(h);
        self.rotation_deg.param_hash(h);
        self.scale.param_hash(h);
        self.scale_x.param_hash(h);
        self.scale_y.param_hash(h);
        h.update(&self.pivot.0.to_le_bytes());
        h.update(&self.pivot.1.to_le_bytes());
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        // The `[x, y]` arrays are literal-only, and seed the per-axis
        // scalars that a caller can drive per render.
        let tx = read_xy(fields, "translate", ctx, [0.0, 0.0])?;
        let pivot = read_xy(fields, "pivot", ctx, [0.0, 0.0])?;

        let mut r = InReader::new(fields, ctx, 1);
        let translate_x = r.number_or("translate-x", tx[0] as f64)?;
        let translate_y = r.number_or("translate-y", tx[1] as f64)?;
        let rotation_deg = r.number_or("rotation-deg", 0.0)?;
        // Uniform `scale` multiplies the per-axis factors, and the literal
        // `scale-xy` array seeds those factors. All three default to 1, so
        // `scale: 0.5` and `scale-xy: [2, 1]` mean exactly what they did
        // before, and `scale-x: "$sx"` now works alongside them.
        let scale_xy = read_xy(fields, "scale-xy", ctx, [1.0, 1.0])?;
        let scale = r.number_or("scale", 1.0)?;
        let scale_x = r.number_or("scale-x", scale_xy[0] as f64)?;
        let scale_y = r.number_or("scale-y", scale_xy[1] as f64)?;
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
            node: Box::new(TransformNode {
                translate_x,
                translate_y,
                rotation_deg,
                scale,
                scale_x,
                scale_y,
                pivot: (pivot[0] as f64, pivot[1] as f64),
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Translate / rotate / scale every input vertex. Rotation happens around `pivot` (defaults to the origin) before the final translation. The scalar fields take a `$param` or an `@node` port and follow it per render; the `[x, y]` arrays are literal, and seed the matching scalars. Uniform `scale` multiplies the per-axis factors.",
            "properties": {
                "features": schema_frag::node_ref(),
                "translate": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "Literal [x, y] shift. `translate-x` / `translate-y` override it." },
                "translate-x": schema_frag::number(),
                "translate-y": schema_frag::number(),
                "rotation-deg": schema_frag::number(),
                "scale": schema_frag::number(),
                "scale-xy": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "Literal per-axis scale. `scale-x` / `scale-y` override it; uniform `scale` multiplies it." },
                "scale-x": schema_frag::number(),
                "scale-y": schema_frag::number(),
                "pivot": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "Literal [x, y] rotation centre in feature space." },
            },
            "required": ["features"],
        })
    }
}

ezu_graph::submit_node!(TransformFactory);
