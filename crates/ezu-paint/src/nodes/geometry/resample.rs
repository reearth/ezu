//! `smooth` / `densify` / `resample` — polyline density adjustment
//! over `Features`. All three operate on every polyline (the
//! `lines` slice) and on each polygon ring (exterior + holes);
//! standalone points are passed through untouched.
//!
//! - `smooth`: Chaikin corner smoothing, `iterations` controls how
//!   many passes (each ~doubles vertex count).
//! - `densify`: insert vertices so no segment exceeds `target-px`.
//! - `resample`: emit evenly-spaced vertices at `spacing-px` along
//!   arc length.

use ezu_features::ops::resample::{densify, resample, smooth};
use ezu_features::Polygon;
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, read_number, read_number_or};

#[derive(Debug, Clone, Copy)]
enum Mode {
    Smooth { iterations: u32 },
    Densify { target_px: f64 },
    Resample { spacing_px: f64 },
}

impl Mode {
    fn apply_open(&self, pts: &[(i32, i32)]) -> Vec<(i32, i32)> {
        match *self {
            Mode::Smooth { iterations } => smooth(pts, iterations, false),
            Mode::Densify { target_px } => densify(pts, target_px, false),
            Mode::Resample { spacing_px } => resample(pts, spacing_px, false),
        }
    }
    fn apply_closed(&self, pts: &[(i32, i32)]) -> Vec<(i32, i32)> {
        match *self {
            Mode::Smooth { iterations } => smooth(pts, iterations, true),
            Mode::Densify { target_px } => densify(pts, target_px, true),
            Mode::Resample { spacing_px } => resample(pts, spacing_px, true),
        }
    }
    fn tag(&self) -> &'static [u8] {
        match self {
            Mode::Smooth { .. } => b"smooth",
            Mode::Densify { .. } => b"densify",
            Mode::Resample { .. } => b"resample",
        }
    }
    fn op_name(&self) -> &'static str {
        match self {
            Mode::Smooth { .. } => "smooth",
            Mode::Densify { .. } => "densify",
            Mode::Resample { .. } => "resample",
        }
    }
}

struct ResampleNode {
    mode: Mode,
}

impl Node for ResampleNode {
    fn op_name(&self) -> &'static str {
        self.mode.op_name()
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
        let lines: Vec<Vec<(i32, i32)>> = feats
            .lines
            .iter()
            .map(|l| self.mode.apply_open(l))
            .collect();
        let polygons: Vec<Polygon> = feats
            .polygons
            .iter()
            .map(|p| Polygon {
                exterior: self.mode.apply_closed(&p.exterior),
                holes: p.holes.iter().map(|h| self.mode.apply_closed(h)).collect(),
            })
            .collect();
        Ok(features_value(
            feats.extent,
            polygons,
            lines,
            feats.points.clone(),
        ))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(self.mode.tag());
        match self.mode {
            Mode::Smooth { iterations } => h.update(&iterations.to_le_bytes()),
            Mode::Densify { target_px } => h.update(&target_px.to_le_bytes()),
            Mode::Resample { spacing_px } => h.update(&spacing_px.to_le_bytes()),
        }
    }
}

pub(super) struct SmoothFactory;
impl NodeFactory for SmoothFactory {
    fn op_name(&self) -> &'static str {
        "smooth"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let iter = (read_number_or(fields, "iterations", ctx, 2.0)? as f64).round();
        if !(0.0..=8.0).contains(&iter) {
            return Err(FactoryError::BadField {
                field: "iterations".into(),
                msg: "expected integer in [0, 8]".into(),
            });
        }
        Ok(BuiltNode {
            node: Box::new(ResampleNode {
                mode: Mode::Smooth {
                    iterations: iter as u32,
                },
            }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Chaikin corner-smoothing over each input polyline and polygon ring. Each iteration roughly doubles the vertex count; 2–3 iterations are usually enough.",
            "properties": {
                "features": schema_frag::node_ref(),
                "iterations": { "type": "integer", "minimum": 0, "maximum": 8, "default": 2 },
            },
            "required": ["features"],
        })
    }
}

pub(super) struct DensifyFactory;
impl NodeFactory for DensifyFactory {
    fn op_name(&self) -> &'static str {
        "densify"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let target_px = read_number(fields, "target-px", ctx)?;
        if target_px <= 0.0 {
            return Err(FactoryError::BadField {
                field: "target-px".into(),
                msg: "must be > 0".into(),
            });
        }
        Ok(BuiltNode {
            node: Box::new(ResampleNode {
                mode: Mode::Densify { target_px },
            }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Insert intermediate vertices so no segment of any polyline or polygon ring exceeds `target-px`. Original vertices are preserved.",
            "properties": {
                "features": schema_frag::node_ref(),
                "target-px": { "type": "number", "minimum": 0.0 },
            },
            "required": ["features", "target-px"],
        })
    }
}

pub(super) struct ResampleFactory;
impl NodeFactory for ResampleFactory {
    fn op_name(&self) -> &'static str {
        "resample"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let spacing_px = read_number(fields, "spacing-px", ctx)?;
        if spacing_px <= 0.0 {
            return Err(FactoryError::BadField {
                field: "spacing-px".into(),
                msg: "must be > 0".into(),
            });
        }
        Ok(BuiltNode {
            node: Box::new(ResampleNode {
                mode: Mode::Resample { spacing_px },
            }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Resample each polyline / polygon ring to evenly-spaced vertices at `spacing-px` along arc length. Original vertices are not preserved.",
            "properties": {
                "features": schema_frag::node_ref(),
                "spacing-px": { "type": "number", "minimum": 0.0 },
            },
            "required": ["features", "spacing-px"],
        })
    }
}

ezu_graph::submit_node!(SmoothFactory);
ezu_graph::submit_node!(DensifyFactory);
ezu_graph::submit_node!(ResampleFactory);
