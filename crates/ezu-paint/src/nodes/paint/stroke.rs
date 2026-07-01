//! `stroke` — `Features -> Raster`. A crisp, constant-width `tiny-skia`
//! vector stroke along polylines, with cap/join and optional dashing. This
//! is the sharp counterpart to `line` (a painterly hokusai brush) — use it
//! to reproduce clean cartographic road/boundary lines (e.g. MapLibre).

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use tiny_skia::{LineCap, LineJoin};
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    canvas_into_raster, color_f32_to_u8, downcast_features, empty_raster, make_canvas,
    read_string_or, tint_alpha_color,
};
use crate::{paint_strokes, StrokeStyle};

struct StrokeNode {
    color: In<[f32; 4]>,
    width_px: In<f64>,
    opacity: In<f64>,
    cap: LineCap,
    join: LineJoin,
    /// On/off dash pattern in pixels (`None` = solid).
    dash: Option<Vec<f32>>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for StrokeNode {
    fn op_name(&self) -> &'static str {
        "stroke"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
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
        if feats.lines.is_empty() {
            return Ok(empty_raster(ctx));
        }
        let rgba8 = color_f32_to_u8(self.color.get(ctx, inputs)?);
        let opacity = self.opacity.get(ctx, inputs)? as f32;
        let color = tint_alpha_color(rgba8, opacity);
        let style = StrokeStyle {
            color,
            width: (self.width_px.get(ctx, inputs)? as f32).max(0.0),
            cap: self.cap,
            join: self.join,
            dash: self.dash.clone(),
        };
        let mut canvas = make_canvas(ctx)?;
        paint_strokes(&mut canvas, &feats.lines, feats.extent, &style);
        Ok(PortValue::Raster(Arc::new(canvas_into_raster(canvas))))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"stroke");
        self.color.param_hash(h);
        self.width_px.param_hash(h);
        self.opacity.param_hash(h);
        h.update(&[cap_tag(self.cap), join_tag(self.join)]);
        if let Some(d) = &self.dash {
            h.update(&[1]);
            for v in d {
                h.update(&v.to_le_bytes());
            }
        } else {
            h.update(&[0]);
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

fn cap_tag(c: LineCap) -> u8 {
    match c {
        LineCap::Butt => 0,
        LineCap::Round => 1,
        LineCap::Square => 2,
    }
}

fn join_tag(j: LineJoin) -> u8 {
    match j {
        LineJoin::Miter => 0,
        LineJoin::Round => 1,
        LineJoin::Bevel => 2,
        LineJoin::MiterClip => 3,
    }
}

pub(super) struct StrokeFactory;
impl NodeFactory for StrokeFactory {
    fn op_name(&self) -> &'static str {
        "stroke"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let cap = match read_string_or(fields, "cap", ctx, "butt")?.as_str() {
            "butt" => LineCap::Butt,
            "round" => LineCap::Round,
            "square" => LineCap::Square,
            other => {
                return Err(FactoryError::BadField {
                    field: "cap".into(),
                    msg: format!("expected butt/round/square, got `{other}`"),
                })
            }
        };
        let join = match read_string_or(fields, "join", ctx, "miter")?.as_str() {
            "miter" => LineJoin::Miter,
            "round" => LineJoin::Round,
            "bevel" => LineJoin::Bevel,
            other => {
                return Err(FactoryError::BadField {
                    field: "join".into(),
                    msg: format!("expected miter/round/bevel, got `{other}`"),
                })
            }
        };
        let dash = match fields.get("dasharray") {
            None | Some(Value::Null) => None,
            Some(v) => {
                let arr = v.as_array().ok_or_else(|| FactoryError::BadField {
                    field: "dasharray".into(),
                    msg: "expected an array of numbers (pixels)".into(),
                })?;
                let pat: Vec<f32> = arr
                    .iter()
                    .filter_map(|x| x.as_f64())
                    .map(|x| x as f32)
                    .collect();
                if pat.is_empty() {
                    None
                } else {
                    Some(pat)
                }
            }
        };

        let mut r = InReader::new(fields, ctx, 1);
        let color = r.color("color")?;
        let width_px = r.number_or("width-px", 1.0)?;
        let opacity = r.number_or("opacity", 1.0)?;
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
            node: Box::new(StrokeNode {
                color,
                width_px,
                opacity,
                cap,
                join,
                dash,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Crisp constant-width vector stroke along feature polylines (tiny-skia), with cap/join and optional pixel `dasharray`. The sharp counterpart to `line` (painterly brush) — for clean cartographic road/boundary lines.",
            "properties": {
                "features": schema_frag::node_ref(),
                "color": schema_frag::color(),
                "width-px": schema_frag::px_number(),
                "opacity": schema_frag::unit_number(),
                "cap": { "type": "string", "enum": ["butt", "round", "square"], "default": "butt" },
                "join": { "type": "string", "enum": ["miter", "round", "bevel"], "default": "miter" },
                "dasharray": { "type": "array", "items": { "type": "number" }, "description": "On/off lengths in pixels; omit for solid." },
            },
            "required": ["features", "color"],
        })
    }
}

ezu_graph::submit_node!(StrokeFactory);
