//! `raster` — `() -> Raster`. Resolves a host-bound RGBA tile mosaic
//! (satellite imagery, pre-rendered basemaps, …) via the unified
//! [`AssetLoader`](ezu_graph::AssetLoader) and emits it as a
//! canvas-padded `Raster` for downstream filters (`posterize`, `hsl`,
//! `blend`, …).
//!
//! The host declares the underlying tile pyramid in the document's
//! `sources` block (`type: "raster"`), fetches + stitches the 3×3
//! neighbourhood, and binds the padded buffer under the source's bare
//! name via `TileLoader::bind_raster` before each render. The style's
//! `raster` node references it via `source: "<name>"`; the field is
//! optional when the document has exactly one `raster` source. An
//! unbound source (404 with `on-missing: empty`) emits transparent
//! pixels.

use std::sync::Arc;

use ezu_graph::{
    Asset, AssetError, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::read_optional_string;

struct RasterNode {
    name: String,
}

impl Node for RasterNode {
    fn op_name(&self) -> &'static str {
        "raster"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Raster
    }
    fn asset_inputs(&self) -> Vec<String> {
        vec![self.name.clone()]
    }
    fn eval(&self, ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        let asset = match ctx.assets.load(&self.name) {
            Ok(a) => a,
            Err(AssetError::NotFound(_)) => {
                // No binding for this tile -> transparent canvas.
                let (pw, ph) = ctx.canvas.padded_dims();
                return Ok(PortValue::Raster(Arc::new(RasterBuf::new(pw, ph))));
            }
            Err(e) => return Err(EvalError::Asset(e)),
        };
        let Asset::Image(buf) = asset else {
            return Err(EvalError::Other(format!(
                "asset `{}` is not an image",
                self.name
            )));
        };
        let (pw, ph) = ctx.canvas.padded_dims();
        if buf.width != pw || buf.height != ph {
            return Err(EvalError::Other(format!(
                "raster source `{}`: bound buffer is {}x{}, expected padded canvas {pw}x{ph}",
                self.name, buf.width, buf.height
            )));
        }
        Ok(PortValue::Raster(buf))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"raster");
        h.update(self.name.as_bytes());
    }
}

pub(super) struct RasterFactory;
impl NodeFactory for RasterFactory {
    fn op_name(&self) -> &'static str {
        "raster"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let name = resolve_raster_source(fields, ctx)?;
        Ok(BuiltNode {
            node: Box::new(RasterNode { name }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Sample a host-bound RGBA tile pyramid (satellite imagery, pre-rendered basemaps) as a canvas-padded Raster. `source` names a `raster` entry in the document's `sources` block; optional when the document declares exactly one such source.",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "Name of a `raster` source in the document's `sources` block. Optional when there is exactly one."
                }
            },
        })
    }
}

fn resolve_raster_source(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
) -> Result<String, FactoryError> {
    if let Some(name) = read_optional_string(fields, "source")? {
        match ctx.sources.get(&name) {
            Some(ezu_style::SourceDecl::Raster(_)) => Ok(name),
            Some(_) => Err(FactoryError::BadField {
                field: "source".into(),
                msg: format!("`{name}` exists but is not a `raster` source"),
            }),
            None => Err(FactoryError::BadField {
                field: "source".into(),
                msg: format!("no source named `{name}` in document"),
            }),
        }
    } else {
        let mut matches = ctx
            .sources
            .iter()
            .filter(|(_, decl)| matches!(decl, ezu_style::SourceDecl::Raster(_)));
        match (matches.next(), matches.next()) {
            (Some((name, _)), None) => Ok(name.clone()),
            (None, _) => Err(FactoryError::BadField {
                field: "source".into(),
                msg: "no `raster` source in document; declare one or pass `source` explicitly"
                    .into(),
            }),
            (Some(_), Some(_)) => Err(FactoryError::BadField {
                field: "source".into(),
                msg: "multiple `raster` sources in document; pass `source` explicitly".into(),
            }),
        }
    }
}

ezu_graph::submit_node!(RasterFactory);
