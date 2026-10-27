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
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, map_with_neighbours, read_number_or, FeatureGroup};

enum Mode {
    Smooth { iterations: u32 },
    Densify { target_px: In<f64> },
    Resample { spacing_px: In<f64> },
}

/// A [`Mode`] with its lengths resolved for one eval.
#[derive(Clone, Copy)]
enum Resolved {
    Smooth { iterations: u32 },
    Densify { target_px: f64 },
    Resample { spacing_px: f64 },
}

impl Resolved {
    /// `px` converts the mode's pixel lengths to feature-extent units, the
    /// space the vertices live in. A non-positive length (only possible
    /// from a port or an unbounded `$param`) leaves the vertices as they
    /// are.
    fn apply(&self, pts: &[(i32, i32)], closed: bool, px: f64) -> Vec<(i32, i32)> {
        match *self {
            Resolved::Smooth { iterations } => smooth(pts, iterations, closed),
            Resolved::Densify { target_px } => densify(pts, target_px * px, closed),
            Resolved::Resample { spacing_px } => resample(pts, spacing_px * px, closed),
        }
    }
}

impl Mode {
    /// Resolve the lengths, from style pixels into device pixels.
    fn resolve(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<Resolved, EvalError> {
        Ok(match self {
            Mode::Smooth { iterations } => Resolved::Smooth {
                iterations: *iterations,
            },
            Mode::Densify { target_px } => Resolved::Densify {
                target_px: ctx.px(target_px.get(ctx, inputs)?),
            },
            Mode::Resample { spacing_px } => Resolved::Resample {
                spacing_px: ctx.px(spacing_px.get(ctx, inputs)?),
            },
        })
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
    /// `features`, then any scalar port a length is wired to.
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl ResampleNode {
    /// Wrap `mode` with its `features` input, followed by whatever scalar
    /// port `parts` gathered for its length.
    fn build(
        features: String,
        mode: Mode,
        parts: ezu_graph::InParts,
    ) -> Result<BuiltNode, FactoryError> {
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
            node: Box::new(ResampleNode {
                mode,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
}

/// Read a required length that must be > 0 wherever that is known at
/// build time: a literal, or a `$param`'s declared `max`.
fn read_positive_length(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
    name: &'static str,
) -> Result<(In<f64>, ezu_graph::InParts), FactoryError> {
    let mut r = InReader::new(fields, ctx, 1);
    let len = r.number(name)?;
    if len.static_bound().is_some_and(|b| b <= 0.0) {
        return Err(FactoryError::BadField {
            field: name.into(),
            msg: "must be > 0".into(),
        });
    }
    Ok((len, r.finish()))
}

impl Node for ResampleNode {
    fn op_name(&self) -> &'static str {
        self.mode.op_name()
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
        map_with_neighbours(ctx, &feats, |ctx, extent, groups| {
            let px = extent as f64 / ctx.canvas.tile_w.max(1) as f64;
            let mode = self.mode.resolve(ctx, inputs)?;
            // Per group: adjust polyline / polygon-ring density (points pass
            // through), carrying properties.
            let mut out_groups = Vec::with_capacity(groups.len());
            for g in groups {
                let lines: Vec<Vec<(i32, i32)>> =
                    g.lines.iter().map(|l| mode.apply(l, false, px)).collect();
                let polygons: Vec<Polygon> = g
                    .polygons
                    .iter()
                    .map(|p| Polygon {
                        exterior: mode.apply(&p.exterior, true, px),
                        holes: p.holes.iter().map(|h| mode.apply(h, true, px)).collect(),
                    })
                    .collect();
                out_groups.push(FeatureGroup {
                    properties: g.properties.clone(),
                    polygons,
                    lines,
                    points: g.points.clone(),
                });
            }
            Ok(out_groups)
        })
    }
    fn forwards_neighbours(&self, port: usize) -> bool {
        port == 0
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(self.mode.tag());
        match &self.mode {
            Mode::Smooth { iterations } => h.update(&iterations.to_le_bytes()),
            // A literal length hashes as the bare number it always did, so
            // existing styles keep their cache keys.
            Mode::Densify { target_px: len } | Mode::Resample { spacing_px: len } => match len {
                In::Const(v) => h.update(&v.to_le_bytes()),
                _ => len.param_hash(h),
            },
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        let iter = read_number_or(fields, "iterations", ctx, 2.0)?.round();
        if !(0.0..=8.0).contains(&iter) {
            return Err(FactoryError::BadField {
                field: "iterations".into(),
                msg: "expected integer in [0, 8]".into(),
            });
        }
        ResampleNode::build(
            features,
            Mode::Smooth {
                iterations: iter as u32,
            },
            ezu_graph::InParts::default(),
        )
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
        let (target_px, parts) = read_positive_length(fields, ctx, "target-px")?;
        ResampleNode::build(features, Mode::Densify { target_px }, parts)
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Insert intermediate vertices so no segment of any polyline or polygon ring exceeds `target-px`. Original vertices are preserved.",
            "properties": {
                "features": schema_frag::node_ref(),
                "target-px": schema_frag::px_number(),
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
        let (spacing_px, parts) = read_positive_length(fields, ctx, "spacing-px")?;
        ResampleNode::build(features, Mode::Resample { spacing_px }, parts)
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Resample each polyline / polygon ring to evenly-spaced vertices at `spacing-px` along arc length. Original vertices are not preserved.",
            "properties": {
                "features": schema_frag::node_ref(),
                "spacing-px": schema_frag::px_number(),
            },
            "required": ["features", "spacing-px"],
        })
    }
}

ezu_graph::submit_node!(SmoothFactory);
ezu_graph::submit_node!(DensifyFactory);
ezu_graph::submit_node!(ResampleFactory);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::geometry::ratio_test::{build, eval, geometry, input, plain, retina};

    #[test]
    fn resample_spacing_is_in_style_pixels() {
        let node = build(
            &ResampleFactory,
            serde_json::json!({ "features": "@f", "spacing-px": 4 }),
        );
        let f = input(FeatureGroup::synthetic(
            vec![],
            vec![vec![(0, 0), (1024, 0), (1024, 640)]],
            vec![],
        ));
        let one = eval(node.as_ref(), plain(), std::slice::from_ref(&f));
        let two = eval(node.as_ref(), retina(), std::slice::from_ref(&f));
        // 4 style px is 64 extent units on either tile.
        let line = one.lines().next().expect("a line");
        assert_eq!((line[0], line[1]), ((0, 0), (64, 0)));
        assert_eq!(geometry(&one), geometry(&two));
    }
}
