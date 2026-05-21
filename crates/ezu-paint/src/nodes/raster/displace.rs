//! `displace` — `Raster + Raster -> Raster`. Photoshop-style
//! displacement map: each output pixel reads from the input at a
//! position offset by the displacement raster's R/G channels.
//!
//! Displacement encoding: R = dx, G = dy, each treated as `[0, 1]`
//! with `0.5` meaning "no offset". The final pixel offset is
//! `(d - 0.5) * 2 * amp_px`. This matches the Adobe convention so a
//! flat 50% gray displacement map is a no-op.
//!
//! Seamlessness across tile borders depends on the displacement
//! source being itself seamless (e.g. `noise` with `anchor: world`);
//! this node grows the upstream pad by `amp_px` so warped samples
//! never escape the available raster on either input.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    read_boundary, read_number, read_number_or, sample_bilinear, BoundaryMode,
};

struct DisplaceNode {
    amp_x: f64,
    amp_y: f64,
    boundary: BoundaryMode,
}

impl Node for DisplaceNode {
    fn op_name(&self) -> &'static str {
        "displace"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[
            PortSpec {
                name: "input",
                kind: PortKind::Raster,
                optional: false,
            },
            PortSpec {
                name: "displacement",
                kind: PortKind::Raster,
                optional: false,
            },
        ];
        SPECS
    }
    fn output(&self) -> PortKind {
        PortKind::Raster
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        let bump = self.amp_x.abs().max(self.amp_y.abs()).ceil() as u32;
        downstream + bump
    }
    fn eval(
        &self,
        _ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let src = inputs[0]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let disp = inputs[1]
            .as_ref()
            .and_then(PortValue::as_raster)
            .ok_or_else(|| EvalError::MissingInput("displacement".into()))?;
        // Output is the same size as the input. Displacement must
        // cover at least that area; if smaller, treat the missing
        // region according to the boundary mode.
        let (w, h) = (src.width, src.height);
        let mut out = RasterBuf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                // Read displacement from the matching pixel of the
                // displacement raster (with boundary fallback if it
                // happens to be smaller than `input`).
                let dpix = if x < disp.width && y < disp.height {
                    disp.pixel(x, y)
                } else {
                    match self.boundary {
                        BoundaryMode::Clamp => disp.pixel(
                            x.min(disp.width.saturating_sub(1)),
                            y.min(disp.height.saturating_sub(1)),
                        ),
                        BoundaryMode::Transparent | BoundaryMode::Mirror => [128, 128, 0, 255],
                    }
                };
                let dx = ((dpix[0] as f64) / 255.0 - 0.5) * 2.0 * self.amp_x;
                let dy = ((dpix[1] as f64) / 255.0 - 0.5) * 2.0 * self.amp_y;
                let sx = x as f64 + dx;
                let sy = y as f64 + dy;
                let px = sample_bilinear(src, sx, sy, self.boundary);
                let i = ((y * w + x) * 4) as usize;
                out.pixels[i..i + 4].copy_from_slice(&px);
            }
        }
        Ok(PortValue::Raster(Arc::new(out)))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"displace");
        h.update(&self.amp_x.to_le_bytes());
        h.update(&self.amp_y.to_le_bytes());
        h.update(&[match self.boundary {
            BoundaryMode::Clamp => 0,
            BoundaryMode::Transparent => 1,
            BoundaryMode::Mirror => 2,
        }]);
    }
}

pub(super) struct DisplaceFactory;
impl NodeFactory for DisplaceFactory {
    fn op_name(&self) -> &'static str {
        "displace"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let disp = take_input_ref(fields, "displacement")?;
        let amp = read_number(fields, "amp-px", ctx)?;
        let amp_x = read_number_or(fields, "amp-x-px", ctx, amp)?;
        let amp_y = read_number_or(fields, "amp-y-px", ctx, amp)?;
        let boundary = read_boundary(fields, "boundary", BoundaryMode::Clamp)?;
        Ok(BuiltNode {
            node: Box::new(DisplaceNode {
                amp_x,
                amp_y,
                boundary,
            }),
            connections: vec![
                Connection {
                    port: "input".into(),
                    src: input,
                },
                Connection {
                    port: "displacement".into(),
                    src: disp,
                },
            ],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Photoshop-style displacement map. Output pixel reads `input` at a position offset by the `displacement` raster's R/G channels (0.5 means no offset). Grows upstream pad by `amp-px` so warped samples stay seamless across tile borders, provided the displacement source is itself seamless (e.g. `noise` with `anchor: world`).",
            "properties": {
                "input": schema_frag::node_ref(),
                "displacement": schema_frag::node_ref(),
                "amp-px": schema_frag::px_number(),
                "amp-x-px": schema_frag::px_number(),
                "amp-y-px": schema_frag::px_number(),
                "boundary": {
                    "type": "string",
                    "enum": ["clamp", "transparent", "mirror"],
                    "default": "clamp",
                },
            },
            "required": ["input", "displacement", "amp-px"],
        })
    }
}

ezu_graph::submit_node!(DisplaceFactory);
