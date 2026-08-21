//! `displace` — Photoshop-style displacement map over `Raster|Sprite`
//! (the main `input` is pass-through; the `displacement` map is also
//! polymorphic but its kind doesn't influence the output kind).
//! Each output pixel reads from `input` at a position offset by the
//! displacement raster's R/G channels.
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
    FactoryError, InReader, Node, NodeFactory, PaddingIn, PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    raster_or_sprite_output, read_boundary, sample_bilinear, unwrap_raster_or_sprite,
    wrap_raster_like, BoundaryMode, ACCEPTS_RASTER_OR_SPRITE,
};

struct DisplaceNode {
    amp_x: PaddingIn,
    amp_y: PaddingIn,
    boundary: BoundaryMode,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for DisplaceNode {
    fn op_name(&self) -> &'static str {
        "displace"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        // Output mirrors the main `input`'s kind; the displacement
        // map's kind is independent.
        raster_or_sprite_output(input_kinds)
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        let bump = self
            .amp_x
            .bound()
            .abs()
            .max(self.amp_y.bound().abs())
            .ceil() as u32;
        downstream + bump
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let (src, kind) = unwrap_raster_or_sprite(input, "input")?;
        let disp_input = inputs[1]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("displacement".into()))?;
        let (disp, _) = unwrap_raster_or_sprite(disp_input, "displacement")?;
        let amp_x = self.amp_x.get(ctx, inputs)?;
        let amp_y = self.amp_y.get(ctx, inputs)?;
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
                let dx = ((dpix[0] as f64) / 255.0 - 0.5) * 2.0 * amp_x;
                let dy = ((dpix[1] as f64) / 255.0 - 0.5) * 2.0 * amp_y;
                let sx = x as f64 + dx;
                let sy = y as f64 + dy;
                let px = sample_bilinear(&src, sx, sy, self.boundary);
                let i = ((y * w + x) * 4) as usize;
                out.pixels[i..i + 4].copy_from_slice(&px);
            }
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"displace");
        self.amp_x.param_hash(h);
        self.amp_y.param_hash(h);
        h.update(&[match self.boundary {
            BoundaryMode::Clamp => 0,
            BoundaryMode::Transparent => 1,
            BoundaryMode::Mirror => 2,
        }]);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
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
        let boundary = read_boundary(fields, "boundary", BoundaryMode::Clamp)?;

        let mut r = InReader::new(fields, ctx, 2);
        // `amp-px` is the shared default for the per-axis amplitudes; when
        // `amp-x-px` / `amp-y-px` are omitted they inherit `amp-px` as-is
        // (same literal / `$param` / `@node` binding).
        let amp = r.number("amp-px")?;
        let amp_x = if fields.contains_key("amp-x-px") {
            r.number("amp-x-px")?
        } else {
            amp.clone()
        };
        let amp_y = if fields.contains_key("amp-y-px") {
            r.number("amp-y-px")?
        } else {
            amp.clone()
        };
        // `amp-px` seeds both axes, so the bound is attached to the
        // resulting value rather than re-read from the field.
        let amp_x = PaddingIn::from_value(amp_x, fields, "amp-x-px")?;
        let amp_y = PaddingIn::from_value(amp_y, fields, "amp-y-px")?;
        let parts = r.finish();

        let mut ports = vec![
            PortSpec {
                name: "input",
                accepts: ACCEPTS_RASTER_OR_SPRITE,
                optional: false,
            },
            PortSpec {
                name: "displacement",
                accepts: ACCEPTS_RASTER_OR_SPRITE,
                optional: false,
            },
        ];
        ports.extend(parts.ports);
        let mut connections = vec![
            Connection {
                port: "input".into(),
                src: input,
            },
            Connection {
                port: "displacement".into(),
                src: disp,
            },
        ];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(DisplaceNode {
                amp_x,
                amp_y,
                boundary,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Photoshop-style displacement map. Output pixel reads `input` at a position offset by the `displacement` raster's R/G channels (0.5 means no offset). Grows upstream pad by `amp-px` so warped samples stay seamless across tile borders, provided the displacement source is itself seamless (e.g. `noise` with `anchor: world`).",
            "properties": {
                "input": schema_frag::node_ref(),
                "displacement": schema_frag::node_ref(),
                "amp-px": schema_frag::px_number(),
                "amp-x-px-max": { "type": "number", "minimum": 0.0, "description": "Upper bound on `amp-x-px` for padding, required when `amp-x-px` is an `@node` port. Values above it are clamped." },
                "amp-y-px-max": { "type": "number", "minimum": 0.0, "description": "Upper bound on `amp-y-px` for padding, required when `amp-y-px` is an `@node` port. Values above it are clamped." },
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
