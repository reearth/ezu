//! `solid` — constant-colour source. Emits a canvas-sized `Raster`
//! by default, or a `Sprite` at the caller-specified `width-px` /
//! `height-px` when `kind: "sprite"`. Sprite mode is convenient as a
//! synthetic source for `stamp` / `tiling` / `place` without going
//! through `image`.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, In, InReader, Node,
    NodeFactory, PortKind, PortSpec, PortValue, RasterBuf, ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::color_to_premul_u8;
use crate::nodes::raster::generator_kind::{
    generator_kind_schema, parse_generator_kind, GeneratorKind,
};

struct SolidNode {
    color: In<[f32; 4]>,
    /// What a `kind: scalar` field holds everywhere.
    value: In<f64>,
    out_kind: GeneratorKind,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for SolidNode {
    fn op_name(&self) -> &'static str {
        "solid"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        self.out_kind.port_kind()
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        if self.out_kind == GeneratorKind::Scalar {
            let (pw, ph) = ctx.canvas.padded_dims();
            let v = self.value.get(ctx, inputs)? as f32;
            return Ok(PortValue::ScalarField(Arc::new(ScalarField {
                width: pw,
                height: ph,
                values: vec![v; (pw * ph) as usize].into(),
                nodata: None,
                geo_scale: None,
            })));
        }
        let rgba = color_to_premul_u8(self.color.get(ctx, inputs)?);
        match self.out_kind {
            GeneratorKind::Scalar => unreachable!("handled above"),
            GeneratorKind::Raster => {
                let (pw, ph) = ctx.canvas.padded_dims();
                Ok(PortValue::Raster(Arc::new(RasterBuf::filled(pw, ph, rgba))))
            }
            GeneratorKind::Sprite { width, height } => Ok(PortValue::Sprite(Arc::new(
                RasterBuf::filled(width, height, rgba),
            ))),
        }
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"solid");
        self.color.param_hash(h);
        if self.out_kind == GeneratorKind::Scalar {
            self.value.param_hash(h);
        }
        let (tag, dims) = self.out_kind.hash_tag();
        h.update(&tag);
        if let Some((w, hh)) = dims {
            h.update(&w.to_le_bytes());
            h.update(&hh.to_le_bytes());
        }
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct SolidFactory;
impl NodeFactory for SolidFactory {
    fn op_name(&self) -> &'static str {
        "solid"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let out_kind = parse_generator_kind(fields, ctx)?;
        let mut r = InReader::new(fields, ctx, 0);
        // A scalar field holds `value`, and a colour fills the others.
        let (color, value) = if out_kind == GeneratorKind::Scalar {
            if fields.contains_key("color") {
                ctx.warn("`color` ignored: `kind: scalar` fills the field with `value`");
            }
            (In::Const([0.0; 4]), r.number_or("value", 1.0)?)
        } else {
            if fields.contains_key("value") {
                ctx.warn("`value` ignored: it is what a `kind: scalar` field holds");
            }
            (r.color("color")?, In::Const(0.0))
        };
        let parts = r.finish();
        Ok(BuiltNode {
            node: Box::new(SolidNode {
                color,
                value,
                out_kind,
                ports: parts.ports,
                param_refs: parts.param_refs,
            }),
            connections: parts.connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Solid-colour source. `kind: raster` (default) fills the full canvas; `kind: sprite` emits a Sprite at `width-px × height-px` for use as a synthetic placement/tiling source; `kind: scalar` emits a canvas-sized ScalarField holding `value` everywhere, a constant operand for `field-math` or a flat stand-in for a DEM.",
            "properties": {
                "color": schema_frag::color(),
                "value": schema_frag::in_number(serde_json::json!({ "type": "number", "default": 1,
                           "description": "`kind: scalar` only: the number the field holds. Default 1." })),
                "kind": generator_kind_schema(),
                "width-px": { "type": "integer", "minimum": 1 },
                "height-px": { "type": "integer", "minimum": 1 },
            },
            "anyOf": [
                { "required": ["color"] },
                { "required": ["kind"], "properties": { "kind": { "const": "scalar" } } },
            ],
        })
    }
}

ezu_graph::submit_node!(SolidFactory);
