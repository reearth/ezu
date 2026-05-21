//! `hatch` — `Features -> Features`. Fill input polygons with a family
//! of parallel lines and emit the clipped segments as polylines. Other
//! geometry types in the input are dropped.

use ezu_features::ops::hatch::{hatch_polygons, HatchOpts};
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError,
    FactoryCtx, FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::common::{downcast_features, features_value, read_number, read_number_or};

struct HatchNode {
    angle_deg: f64,
    spacing: f64,
    phase: f64,
}

impl Node for HatchNode {
    fn op_name(&self) -> &'static str {
        "hatch"
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
        let lines = hatch_polygons(
            &feats.polygons,
            &HatchOpts {
                angle_deg: self.angle_deg,
                spacing: self.spacing,
                phase: self.phase,
            },
        );
        Ok(features_value(feats.extent, vec![], lines, vec![]))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"hatch");
        h.update(&self.angle_deg.to_le_bytes());
        h.update(&self.spacing.to_le_bytes());
        h.update(&self.phase.to_le_bytes());
    }
}

pub(super) struct HatchFactory;
impl NodeFactory for HatchFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let angle_deg = read_number_or(fields, "angle-deg", ctx, 0.0)?;
        let spacing = read_number(fields, "spacing", ctx)?;
        let phase = read_number_or(fields, "phase", ctx, 0.0)?;
        Ok(BuiltNode {
            node: Box::new(HatchNode {
                angle_deg,
                spacing,
                phase,
            }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Parallel-line hatching of input polygons.",
            "properties": {
                "features": schema_frag::node_ref(),
                "angle-deg": { "type": "number", "default": 0.0,
                                "description": "Hatch direction (degrees, CCW from +X)." },
                "spacing": { "type": "number", "minimum": 0.0,
                              "description": "Perpendicular spacing between consecutive lines, in tile pixels." },
                "phase": { "type": "number", "default": 0.0,
                            "description": "Per-line offset in spacing units (0..1 cycles through one period)." },
            },
            "required": ["features", "spacing"],
        })
    }
}
