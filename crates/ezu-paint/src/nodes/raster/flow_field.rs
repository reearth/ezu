//! `flow-field` — `ScalarField -> Raster`. Turns an elevation field into
//! a per-pixel direction: down the fall line, up it, or along the contour
//! lines. Built for `flow-smear`, which streaks a texture along the
//! field, and readable by `displace` as it stands.
//!
//! Encoding, the same as `displace`'s: `R = 0.5 + 0.5·vx`,
//! `G = 0.5 + 0.5·vy`, in pixel axes (+x east / right, +y south / down),
//! `B = 0`, `A = 1`. A flat 50% grey means no direction. The vector's
//! length is the slope angle over `max-deg`, capped at 1, so flat ground
//! is 0 and anything at or past `max-deg` is 1; `normalize` makes every
//! sloping pixel length 1 instead.
//!
//! `along` is the downhill vector turned so that downhill lies on its
//! right-hand side: on a north-up map it runs counter-clockwise around a
//! summit and clockwise around a hollow.
//!
//! The gradient is the same 3×3 Horn window `slope` and `hillshade` use,
//! scaled by the field's metres per pixel; a field without geographic
//! scale gets pixel-space gradients, as it does there.
//!
//! Missing samples (NaN, or equal to the field's `nodata`) get the
//! no-flow 50% grey, opaque like every other pixel, so `displace` and
//! `flow-smear` leave them in place. A missing neighbour counts as the
//! centre value, so the ground around a hole does not flow into it or
//! out of it.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::terrain_common::horn_gradient;
use crate::nodes::common::read_string_or;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Downhill,
    Uphill,
    Along,
}

struct FlowFieldNode {
    direction: Direction,
    max_deg: In<f64>,
    normalize: In<bool>,
    exaggeration: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for FlowFieldNode {
    fn op_name(&self) -> &'static str {
        "flow-field"
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
        let field = inputs[0]
            .as_ref()
            .and_then(PortValue::as_scalar_field)
            .ok_or_else(|| EvalError::MissingInput("field".into()))?;
        let max_deg = self.max_deg.get(ctx, inputs)? as f32;
        let normalize = self.normalize.get(ctx, inputs)?;
        let exaggeration = self.exaggeration.get(ctx, inputs)? as f32;
        let w = field.width;
        let h = field.height;
        let mut out = RasterBuf::new(w, h);
        let inv_x = exaggeration / (8.0 * field.metres_per_pixel_x().max(1e-6));
        let inv_y = exaggeration / (8.0 * field.metres_per_pixel_y().max(1e-6));
        let max_rad = max_deg.to_radians().max(1e-4);
        for y in 0..h {
            for x in 0..w {
                // Horn's dz/dy grows southwards, the same way as the
                // pixel rows, so the gradient is already in output axes.
                // A missing sample has no slope to follow, so it gets
                // the no-flow vector, the same as flat ground.
                let (dz_dx, dz_dy) = horn_gradient(field, x, y, inv_x, inv_y).unwrap_or((0.0, 0.0));
                let g = (dz_dx * dz_dx + dz_dy * dz_dy).sqrt();
                let (vx, vy) = if g > 0.0 {
                    let len = if normalize {
                        1.0
                    } else {
                        (libm::atanf(g) / max_rad).min(1.0)
                    };
                    // Unit downhill vector, scaled.
                    let dx = -dz_dx / g * len;
                    let dy = -dz_dy / g * len;
                    match self.direction {
                        Direction::Downhill => (dx, dy),
                        Direction::Uphill => (-dx, -dy),
                        // With y down, the right-hand side of heading
                        // (a, b) is (-b, a); setting that to downhill
                        // gives (dy, -dx).
                        Direction::Along => (dy, -dx),
                    }
                } else {
                    (0.0, 0.0)
                };
                let i = ((y * w + x) * 4) as usize;
                out.pixels[i] = encode(vx);
                out.pixels[i + 1] = encode(vy);
                out.pixels[i + 2] = 0;
                out.pixels[i + 3] = 255;
            }
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"flow-field");
        h.update(match self.direction {
            Direction::Downhill => b"dn",
            Direction::Uphill => b"up",
            Direction::Along => b"al",
        });
        self.max_deg.param_hash(h);
        self.normalize.param_hash(h);
        self.exaggeration.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

/// One vector component in `[-1, 1]` as a `0.5`-centred channel byte.
#[inline]
fn encode(v: f32) -> u8 {
    ((0.5 + 0.5 * v) * 255.0).round().clamp(0.0, 255.0) as u8
}

pub(super) struct FlowFieldFactory;
impl NodeFactory for FlowFieldFactory {
    fn op_name(&self) -> &'static str {
        "flow-field"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "field")?;
        let direction = match read_string_or(fields, "direction", ctx, "downhill")?.as_str() {
            "downhill" => Direction::Downhill,
            "uphill" => Direction::Uphill,
            "along" => Direction::Along,
            other => {
                return Err(FactoryError::BadField {
                    field: "direction".into(),
                    msg: format!("expected `downhill`, `uphill` or `along`, got `{other}`"),
                });
            }
        };
        let mut r = InReader::new(fields, ctx, 1);
        let max_deg = r.number_or("max-deg", 45.0)?;
        let normalize = r.bool_or("normalize", false)?;
        let exaggeration = r.number_or("exaggeration", 1.0)?;
        let parts = r.finish();

        let mut ports = vec![PortSpec {
            name: "field",
            accepts: &[PortKind::ScalarField],
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "field".into(),
            src: input,
        }];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(FlowFieldNode {
                direction,
                max_deg,
                normalize,
                exaggeration,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Direction raster from an elevation field: which way the ground falls, rises, or runs level at each pixel. R = 0.5 + 0.5·vx and G = 0.5 + 0.5·vy in pixel axes (+x east, +y south), B = 0, opaque; flat 50% grey means no direction. This is `displace`'s encoding, so the output feeds `displace` directly, and it is what `flow-smear` reads to streak a texture along the terrain. The vector's length is the slope angle over `max-deg` (capped at 1), or 1 everywhere the ground slopes with `normalize`. Missing samples (NaN or the field's nodata value) get the opaque no-flow grey, so `displace` and `flow-smear` leave them in place; a missing neighbour counts as the centre value, so the ground around a hole does not flow into or out of it.",
            "properties": {
                "field": schema_frag::node_ref(),
                "direction": { "type": "string", "enum": ["downhill", "uphill", "along"], "default": "downhill",
                               "description": "`downhill` follows the fall line, `uphill` points the other way, and `along` follows the contour lines with downhill on its right-hand side (counter-clockwise around a summit on a north-up map)." },
                "max-deg": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 45,
                              "description": "Slope angle (degrees) at which the vector reaches full length. Ignored with `normalize`." })),
                "normalize": { "oneOf": [{"type": "boolean"}, {"type": "string", "pattern": "^[$@].+"}], "default": false,
                               "description": "If true, every sloping pixel gets a vector of length 1; only perfectly flat ground stays at zero." },
                "exaggeration": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 1.0,
                                  "description": "Multiplier on the gradient before the slope angle is taken, as in `hillshade`." })),
            },
            "required": ["field"],
        })
    }
}

ezu_graph::submit_node!(FlowFieldFactory);
