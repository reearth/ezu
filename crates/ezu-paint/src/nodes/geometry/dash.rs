//! `dash` — `Features -> Features`. Cut each input polyline into a
//! series of shorter polylines following a `dash-px` / `gap-px` /
//! `phase-px` pattern. Polygons and points pass through untouched.
//!
//! Walking is performed in tile-pixel space (consistent with other
//! `*-px` parameters in the codebase): pixel lengths are converted to
//! feature-extent units using `extent / tile-size` at eval time.

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, read_number, read_number_or};

struct DashNode {
    dash_px: f64,
    gap_px: f64,
    phase_px: f64,
}

impl Node for DashNode {
    fn op_name(&self) -> &'static str {
        "dash"
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
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let feats = downcast_features(
            inputs[0]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("features".into()))?,
        )?;
        let scale = feats.extent as f64 / ctx.canvas.tile_size.max(1) as f64;
        let dash = self.dash_px * scale;
        let gap = self.gap_px * scale;
        let phase = self.phase_px * scale;

        let mut out_lines: Vec<Vec<(i32, i32)>> = Vec::new();
        for line in &feats.lines {
            dash_polyline(line, dash, gap, phase, &mut out_lines);
        }
        Ok(features_value(
            feats.extent,
            feats.polygons.clone(),
            out_lines,
            feats.points.clone(),
        ))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"dash");
        h.update(&self.dash_px.to_le_bytes());
        h.update(&self.gap_px.to_le_bytes());
        h.update(&self.phase_px.to_le_bytes());
    }
}

/// Walk `line` (in extent units) and push each contiguous "dash" run
/// into `out` as its own polyline. `dash`/`gap`/`phase` are also in
/// extent units. Degenerate cases (`dash<=0` → no output; `gap<=0` →
/// pass through; period 0 → pass through).
fn dash_polyline(
    line: &[(i32, i32)],
    dash: f64,
    gap: f64,
    phase: f64,
    out: &mut Vec<Vec<(i32, i32)>>,
) {
    if line.len() < 2 {
        return;
    }
    if dash <= 0.0 {
        return;
    }
    let period = dash + gap;
    if gap <= 0.0 || period <= 0.0 {
        out.push(line.to_vec());
        return;
    }
    // `state` ∈ [0, period); in-dash iff state < dash.
    let mut state = phase.rem_euclid(period);
    let mut cur: Vec<(f64, f64)> = Vec::new();

    let mut x = line[0].0 as f64;
    let mut y = line[0].1 as f64;
    if state < dash {
        cur.push((x, y));
    }

    for win in line.windows(2) {
        let (xn, yn) = (win[1].0 as f64, win[1].1 as f64);
        let dx = xn - x;
        let dy = yn - y;
        let seg = (dx * dx + dy * dy).sqrt();
        if seg <= 0.0 {
            x = xn;
            y = yn;
            continue;
        }
        let ux = dx / seg;
        let uy = dy / seg;
        let mut walked = 0.0;
        while walked < seg {
            let rem = seg - walked;
            let in_dash = state < dash;
            let to_boundary = if in_dash {
                dash - state
            } else {
                period - state
            };
            let step = to_boundary.min(rem).max(0.0);
            let nx = x + ux * step;
            let ny = y + uy * step;
            if in_dash {
                if cur.is_empty() {
                    cur.push((x, y));
                }
                cur.push((nx, ny));
            }
            x = nx;
            y = ny;
            walked += step;
            state += step;
            if state >= period {
                state -= period;
            }
            // If we just left a dash run, flush it.
            if in_dash && state >= dash - 1e-9 {
                if cur.len() >= 2 {
                    out.push(quantize(&cur));
                }
                cur.clear();
            }
            if step <= 0.0 {
                break; // safety: avoid infinite loop on zero-step edge cases
            }
        }
    }
    if cur.len() >= 2 {
        out.push(quantize(&cur));
    }
}

fn quantize(pts: &[(f64, f64)]) -> Vec<(i32, i32)> {
    let mut out = Vec::with_capacity(pts.len());
    let mut last: Option<(i32, i32)> = None;
    for &(x, y) in pts {
        let q = (x.round() as i32, y.round() as i32);
        if Some(q) != last {
            out.push(q);
            last = Some(q);
        }
    }
    out
}

pub(super) struct DashFactory;
impl NodeFactory for DashFactory {
    fn op_name(&self) -> &'static str {
        "dash"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let dash_px = read_number(fields, "dash-px", ctx)?;
        let gap_px = read_number(fields, "gap-px", ctx)?;
        let phase_px = read_number_or(fields, "phase-px", ctx, 0.0)?;
        Ok(BuiltNode {
            node: Box::new(DashNode {
                dash_px,
                gap_px,
                phase_px,
            }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Cut polylines into dash/gap segments. Lengths are in canvas pixels (consistent with `*-px` brush parameters). Polygons and points pass through unchanged.",
            "properties": {
                "features": schema_frag::node_ref(),
                "dash-px": schema_frag::px_number(),
                "gap-px": schema_frag::px_number(),
                "phase-px": { "type": "number",
                              "description": "Initial offset into the dash/gap pattern, in pixels. May be negative." },
            },
            "required": ["features", "dash-px", "gap-px"],
        })
    }
}

ezu_graph::submit_node!(DashFactory);
