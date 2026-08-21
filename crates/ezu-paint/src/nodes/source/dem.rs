//! `dem` — `() -> ScalarField`. Resolves a host-bound DEM mosaic via
//! the unified [`AssetLoader`](ezu_graph::AssetLoader) and emits it as a
//! `ScalarField` port value for `hillshade` / `slope` / `color-ramp`.
//!
//! The host is expected to declare the underlying tile source in the
//! style document's `sources` block, fetch + stitch the tiles, and bind
//! the resulting [`ScalarField`] under the source's bare name via
//! `TileLoader::bind_scalar_field` before each render. The style's
//! `dem` node references it via `source: "<name>"` matching the
//! document's `sources` entry; the field is optional when the
//! document has exactly one `dem` source.

use std::sync::Arc;

use ezu_graph::{
    Asset, AssetError, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue, ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::read_optional_string;

struct DemNode {
    name: String,
}

impl Node for DemNode {
    fn op_name(&self) -> &'static str {
        "dem"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::ScalarField
    }
    fn asset_inputs(&self) -> Vec<String> {
        vec![self.name.clone()]
    }
    fn eval(&self, ctx: &EvalCtx<'_>, _: &[Option<PortValue>]) -> Result<PortValue, EvalError> {
        let asset = match ctx.assets.load(&self.name) {
            Ok(a) => a,
            Err(AssetError::NotFound(_)) => {
                // No binding for this tile -> emit a zero field sized to
                // the canvas. Consumers degrade gracefully (hillshade
                // becomes flat-lit, slope is zero).
                let (pw, ph) = ctx.canvas.padded_dims();
                let count = (pw * ph) as usize;
                return Ok(PortValue::ScalarField(Arc::new(ScalarField {
                    width: pw,
                    height: ph,
                    values: vec![0.0; count].into(),
                    nodata: None,
                    geo_scale: None,
                })));
            }
            Err(e) => return Err(EvalError::Asset(e)),
        };
        let Asset::ScalarField(field) = asset else {
            return Err(EvalError::Other(format!(
                "asset `{}` is not a scalar field",
                self.name
            )));
        };
        Ok(PortValue::ScalarField(field))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"dem");
        h.update(self.name.as_bytes());
    }
}

pub(super) struct DemFactory;
impl NodeFactory for DemFactory {
    fn op_name(&self) -> &'static str {
        "dem"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let name = resolve_dem_source(fields, ctx)?;
        Ok(BuiltNode {
            node: Box::new(DemNode { name }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Sample a host-bound raster DEM as a ScalarField. `source` names a `dem` entry in the document's `sources` block; optional when the document declares exactly one such source.",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "Name of a `dem` source in the document's `sources` block. Optional when there is exactly one."
                }
            },
        })
    }
}

fn resolve_dem_source(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
) -> Result<String, FactoryError> {
    if let Some(name) = read_optional_string(fields, "source")? {
        match ctx.sources.get(&name) {
            Some(ezu_style::SourceDecl::Dem(_)) => Ok(name),
            Some(_) => Err(FactoryError::BadField {
                field: "source".into(),
                msg: format!("`{name}` exists but is not a `dem` source"),
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
            .filter(|(_, decl)| matches!(decl, ezu_style::SourceDecl::Dem(_)));
        match (matches.next(), matches.next()) {
            (Some((name, _)), None) => Ok(name.clone()),
            (None, _) => Err(FactoryError::BadField {
                field: "source".into(),
                msg: "no `dem` source in document; declare one or pass `source` explicitly".into(),
            }),
            (Some(_), Some(_)) => Err(FactoryError::BadField {
                field: "source".into(),
                msg: "multiple `dem` sources in document; pass `source` explicitly".into(),
            }),
        }
    }
}

ezu_graph::submit_node!(DemFactory);
