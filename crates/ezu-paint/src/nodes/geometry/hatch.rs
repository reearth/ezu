//! `hatch` — `Features -> Features`. Fill input polygons with a family
//! of parallel lines and emit the clipped segments as polylines. Other
//! geometry types in the input are dropped.

use ezu_features::ops::hatch::{hatch_polygons, HatchOpts};
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, FeatureGroup};

struct HatchNode {
    angle_deg: In<f64>,
    spacing: In<f64>,
    phase: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for HatchNode {
    fn op_name(&self) -> &'static str {
        "hatch"
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
        // World origin of this tile, in feature-extent units, so the
        // line family is laid out on a global grid and adjacent tiles
        // agree at the seam.
        let extent = feats.extent as f64;
        let origin = (ctx.tile.x as f64 * extent, ctx.tile.y as f64 * extent);
        // `spacing` is in style pixels: in device pixels first, then in
        // extent units, which come out the same at every pixel ratio so a
        // retina tile hatches the very same world lines.
        let px = extent / ctx.canvas.tile_w.max(1) as f64;
        let opts = HatchOpts {
            angle_deg: self.angle_deg.get(ctx, inputs)?,
            spacing: ctx.px(self.spacing.get(ctx, inputs)?) * px,
            phase: self.phase.get(ctx, inputs)?,
            origin,
        };
        // Per group: hatch each feature's polygons into polylines, carrying
        // properties.
        let mut out_groups = Vec::with_capacity(feats.groups.len());
        for g in &feats.groups {
            let lines = hatch_polygons(&g.polygons, &opts);
            out_groups.push(FeatureGroup {
                properties: g.properties.clone(),
                polygons: vec![],
                lines,
                points: vec![],
            });
        }
        Ok(features_value(feats.extent, out_groups))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"hatch");
        self.angle_deg.param_hash(h);
        self.spacing.param_hash(h);
        self.phase.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct HatchFactory;
impl NodeFactory for HatchFactory {
    fn op_name(&self) -> &'static str {
        "hatch"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let mut r = InReader::new(fields, ctx, 1);
        let angle_deg = r.number_or("angle-deg", 0.0)?;
        let spacing = r.number("spacing")?;
        let phase = r.number_or("phase", 0.0)?;
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
            node: Box::new(HatchNode {
                angle_deg,
                spacing,
                phase,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Parallel-line hatching of input polygons.",
            "properties": {
                "features": schema_frag::node_ref(),
                "angle-deg": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.0,
                                "description": "Hatch direction (degrees, CCW from +X)." })),
                "spacing": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0,
                              "description": "Perpendicular spacing between consecutive lines, in tile pixels." })),
                "phase": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 0.0,
                            "description": "Per-line offset in spacing units (0..1 cycles through one period)." })),
            },
            "required": ["features", "spacing"],
        })
    }
}

ezu_graph::submit_node!(HatchFactory);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::geometry::ratio_test::{build, eval, geometry, input, plain, retina};
    use ezu_features::Polygon;

    #[test]
    fn hatch_spacing_is_in_style_pixels() {
        let node = build(
            &HatchFactory,
            serde_json::json!({ "features": "@f", "spacing": 8, "angle-deg": 0 }),
        );
        let square = Polygon {
            exterior: vec![(0, 0), (4096, 0), (4096, 4096), (0, 4096)],
            holes: vec![],
        };
        let f = input(FeatureGroup::synthetic(vec![square], vec![], vec![]));
        let one = eval(node.as_ref(), plain(), std::slice::from_ref(&f));
        let two = eval(node.as_ref(), retina(), std::slice::from_ref(&f));
        // 8 style px is 8 device px on a 256 px tile and 16 on a 512 px
        // one: 128 extent units either way, so the very same lines.
        let n = one.lines().count();
        assert!((31..=33).contains(&n), "{n} lines 128 units apart");
        assert_eq!(geometry(&one), geometry(&two));
    }
}
