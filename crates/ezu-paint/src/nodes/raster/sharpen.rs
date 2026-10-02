//! `sharpen` — `Raster|Sprite|ScalarField` pass-through (the output
//! kind mirrors the input). Classic 4-neighbour Laplacian sharpen: each
//! pixel is amplified relative to its orthogonal neighbours by
//! `amount`. With `amount = 0` it's a no-op; around `1.0` it's a
//! typical "unsharp mask" look. Grows upstream pad by 1 so the 3-tap
//! kernel stays in-bounds at tile borders.
//!
//! A `ScalarField` is sharpened in `f32`, unclamped, and keeps its
//! `nodata` and `geo_scale`. On an elevation field that steepens the
//! flanks of ridges and valleys, so `hillshade` draws crisper crests
//! and `slope` reads them as steeper. A `nodata` neighbour counts as
//! the centre value, and a `nodata` centre stays `nodata`.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, Node, NodeFactory, PortKind, PortSpec, PortValue, RasterBuf,
    ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    image_or_field_output, unwrap_raster_or_sprite, wrap_raster_like, ACCEPTS_IMAGE_OR_FIELD,
};

struct SharpenNode {
    amount: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for SharpenNode {
    fn op_name(&self) -> &'static str {
        "sharpen"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, input_kinds: &[Option<PortKind>]) -> PortKind {
        image_or_field_output(input_kinds)
    }
    fn required_pad(&self, downstream: u32) -> u32 {
        downstream + 1
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let input = inputs[0]
            .as_ref()
            .ok_or_else(|| EvalError::MissingInput("input".into()))?;
        let amount = self.amount.get(ctx, inputs)? as f32;
        if let Some(field) = input.as_scalar_field() {
            if amount.abs() < 1e-6 || field.width == 0 || field.height == 0 {
                return Ok(input.clone());
            }
            let mut values = vec![0f32; field.values.len()];
            crate::imaging::laplacian_sharpen_field(
                &field.values,
                &mut values,
                field.width as usize,
                field.height as usize,
                amount,
                field.nodata,
            );
            return Ok(PortValue::ScalarField(Arc::new(ScalarField {
                values: values.into(),
                ..**field
            })));
        }
        let (src, kind) = unwrap_raster_or_sprite(input, "input")?;
        if amount.abs() < 1e-6 {
            return Ok(wrap_raster_like(src, kind));
        }
        let w = src.width;
        let h = src.height;
        // Convolution with the cross Laplacian:
        //     0  -k   0
        //    -k 1+4k -k
        //     0  -k   0
        // Applied per channel on premultiplied data, then clamped.
        // Premultiplied is fine here: the kernel is a linear filter
        // and stays consistent across alpha values.
        let sample = |x: i32, y: i32, c: usize| -> i32 {
            let xc = x.clamp(0, w as i32 - 1) as u32;
            let yc = y.clamp(0, h as i32 - 1) as u32;
            src.pixels[((yc * w + xc) * 4) as usize + c] as i32
        };
        let mut out = RasterBuf::new(w, h);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let off = ((y as u32 * w + x as u32) * 4) as usize;
                for c in 0..4 {
                    let centre = sample(x, y, c) as f32;
                    let neigh = sample(x - 1, y, c) as f32
                        + sample(x + 1, y, c) as f32
                        + sample(x, y - 1, c) as f32
                        + sample(x, y + 1, c) as f32;
                    let v = centre * (1.0 + 4.0 * amount) - neigh * amount;
                    out.pixels[off + c] = v.clamp(0.0, 255.0) as u8;
                }
            }
        }
        Ok(wrap_raster_like(Arc::new(out), kind))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"sharpen");
        self.amount.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct SharpenFactory;
impl NodeFactory for SharpenFactory {
    fn op_name(&self) -> &'static str {
        "sharpen"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let input = take_input_ref(fields, "input")?;
        let mut r = InReader::new(fields, ctx, 1);
        let amount = r.number_or("amount", 0.5)?;
        let parts = r.finish();

        let mut ports = vec![PortSpec {
            name: "input",
            accepts: ACCEPTS_IMAGE_OR_FIELD,
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "input".into(),
            src: input,
        }];
        connections.extend(parts.connections);

        Ok(BuiltNode {
            node: Box::new(SharpenNode {
                amount,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "4-neighbour Laplacian sharpen on a raster, sprite or scalar field; the output is the same kind as the input. Amplifies each pixel relative to its orthogonal neighbours by `amount`. Around 0.5–1.0 is a typical unsharp-mask look; negative values give a soft halo. Sharpening an elevation field (`dem`) before `hillshade` or `slope` steepens the flanks of ridges and valleys, so the shading draws crisper crests; a field is not clamped, so values can overshoot past the field's range next to a sharp step. A field keeps its nodata value and geographic scale; a nodata neighbour counts as the centre value, so a hole does not distort its rim, and a nodata sample stays nodata. Grows upstream pad by 1.",
            "properties": {
                "input": schema_frag::node_ref(),
                "amount": schema_frag::in_number(serde_json::json!({
                    "type": "number", "default": 0.5,
                    "description": "Sharpening strength. 0 = pass-through, ~1 = strong."
                })),
            },
            "required": ["input"],
        })
    }
}

ezu_graph::submit_node!(SharpenFactory);
