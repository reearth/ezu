//! `solid` — constant-colour source. Emits a canvas-sized `Raster`
//! by default, or a `Sprite` at the caller-specified `width-px` /
//! `height-px` when `kind: "sprite"`. Sprite mode is convenient as a
//! synthetic source for `stamp` / `tiling` / `place` without going
//! through `image`.

use std::sync::Arc;

use ezu_graph::{
    schema_frag, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{color_to_premul_u8, read_color};
use crate::nodes::raster::generator_kind::{parse_generator_kind, GeneratorKind};

struct SolidNode {
    color: [f32; 4],
    out_kind: GeneratorKind,
}

impl Node for SolidNode {
    fn op_name(&self) -> &'static str {
        "solid"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        match self.out_kind {
            GeneratorKind::Raster => PortKind::Raster,
            GeneratorKind::Sprite { .. } => PortKind::Sprite,
        }
    }
    fn eval(&self, ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        let rgba = color_to_premul_u8(self.color);
        match self.out_kind {
            GeneratorKind::Raster => {
                let size = ctx.canvas.padded_size();
                Ok(PortValue::Raster(Arc::new(RasterBuf::filled(
                    size, size, rgba,
                ))))
            }
            GeneratorKind::Sprite { width, height } => Ok(PortValue::Sprite(Arc::new(
                RasterBuf::filled(width, height, rgba),
            ))),
        }
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"solid");
        for c in self.color {
            h.update(&c.to_le_bytes());
        }
        let (tag, dims) = self.out_kind.hash_tag();
        h.update(&tag);
        if let Some((w, hh)) = dims {
            h.update(&w.to_le_bytes());
            h.update(&hh.to_le_bytes());
        }
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
        let color = read_color(fields, "color", ctx)?;
        let out_kind = parse_generator_kind(fields, ctx)?;
        Ok(BuiltNode {
            node: Box::new(SolidNode { color, out_kind }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Solid-colour source. `kind: raster` (default) fills the full canvas; `kind: sprite` emits a Sprite at `width-px × height-px` for use as a synthetic placement/tiling source.",
            "properties": {
                "color": schema_frag::color(),
                "kind": { "type": "string", "enum": ["raster", "sprite"], "default": "raster" },
                "width-px": { "type": "integer", "minimum": 1 },
                "height-px": { "type": "integer", "minimum": 1 },
            },
            "required": ["color"],
        })
    }
}

ezu_graph::submit_node!(SolidFactory);
