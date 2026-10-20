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
//! `raster` node references it via `source: "<name>"`; omitting the
//! field falls back to the document's only `raster` source and warns.
//! An
//! unbound source (404 with `on-missing: empty`) emits transparent
//! pixels, and so does a tile outside the node's `min-zoom`/`max-zoom`.

use std::sync::Arc;

use ezu_graph::{
    Asset, AssetError, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue, RasterBuf,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{read_optional_zoom, resolve_source};

struct RasterNode {
    name: String,
    min_zoom: Option<u8>,
    max_zoom: Option<u8>,
}

/// What an absent source draws: transparent pixels over the padded canvas.
fn transparent(ctx: &EvalCtx<'_>) -> PortValue {
    let (pw, ph) = ctx.canvas.padded_dims();
    PortValue::Raster(Arc::new(RasterBuf::new(pw, ph)))
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
        // Outside the node's zoom band it draws what an absent source
        // does, without looking the binding up — as `features` does.
        let z = ctx.tile.z;
        if self.min_zoom.is_some_and(|mn| z < mn) || self.max_zoom.is_some_and(|mx| z > mx) {
            return Ok(transparent(ctx));
        }
        let asset = match ctx.assets.load(&self.name) {
            Ok(a) => a,
            // No binding for this tile -> transparent canvas.
            Err(AssetError::NotFound(_)) => return Ok(transparent(ctx)),
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
        if let Some(z) = self.min_zoom {
            h.update(b"minz");
            h.update(&[z]);
        }
        if let Some(z) = self.max_zoom {
            h.update(b"maxz");
            h.update(&[z]);
        }
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
            node: Box::new(RasterNode {
                name,
                min_zoom: read_optional_zoom(fields, "min-zoom")?,
                max_zoom: read_optional_zoom(fields, "max-zoom")?,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Sample a host-bound RGBA tile pyramid (satellite imagery, pre-rendered basemaps) as a canvas-padded Raster. `source` names a `raster` entry in the document's `sources` block; omitting it falls back to the document's only `raster` source, with a build warning.",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "Name of a `raster` source in the document's `sources` block. Omitting it resolves to the only such source and warns at build time."
                },
                "min-zoom": { "type": "integer", "minimum": 0, "maximum": 24,
                              "description": "Style-level minimum zoom. Below this zoom the node emits transparent pixels, as for a tile the source does not have." },
                "max-zoom": { "type": "integer", "minimum": 0, "maximum": 24,
                              "description": "Style-level maximum zoom. Above this zoom the node emits transparent pixels." }
            },
        })
    }
}

fn resolve_raster_source(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
) -> Result<String, FactoryError> {
    resolve_source(fields, ctx, "`raster`", |decl| {
        matches!(decl, ezu_style::SourceDecl::Raster(_))
    })
}

ezu_graph::submit_node!(RasterFactory);
