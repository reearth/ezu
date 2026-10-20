//! `dem` — `() -> ScalarField`. Resolves a host-bound DEM mosaic via
//! the unified [`AssetLoader`](ezu_graph::AssetLoader) and emits it as a
//! `ScalarField` port value for `hillshade` / `slope` / `color-ramp`.
//!
//! The host is expected to declare the underlying tile source in the
//! style document's `sources` block, fetch + stitch the tiles, and bind
//! the resulting [`ScalarField`] under the source's bare name via
//! `TileLoader::bind_scalar_field` before each render. The style's
//! `dem` node references it via `source: "<name>"` matching the
//! document's `sources` entry; omitting the field falls back to the
//! document's only `dem` source and warns at build time.

use std::sync::Arc;

use ezu_graph::{
    Asset, AssetError, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue, ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{read_optional_zoom, resolve_source};

struct DemNode {
    name: String,
    min_zoom: Option<u8>,
    max_zoom: Option<u8>,
}

/// What an absent source yields: a zero field sized to the padded canvas.
/// Consumers degrade gracefully (hillshade becomes flat-lit, slope is
/// zero).
fn flat(ctx: &EvalCtx<'_>) -> PortValue {
    let (pw, ph) = ctx.canvas.padded_dims();
    let count = (pw * ph) as usize;
    PortValue::ScalarField(Arc::new(ScalarField {
        width: pw,
        height: ph,
        values: vec![0.0; count].into(),
        nodata: None,
        geo_scale: None,
    }))
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
        // Outside the node's zoom band it yields what an absent source
        // does, without looking the binding up — as `features` does.
        let z = ctx.tile.z;
        if self.min_zoom.is_some_and(|mn| z < mn) || self.max_zoom.is_some_and(|mx| z > mx) {
            return Ok(flat(ctx));
        }
        let asset = match ctx.assets.load(&self.name) {
            Ok(a) => a,
            // No binding for this tile -> a zero field.
            Err(AssetError::NotFound(_)) => return Ok(flat(ctx)),
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
            node: Box::new(DemNode {
                name,
                min_zoom: read_optional_zoom(fields, "min-zoom")?,
                max_zoom: read_optional_zoom(fields, "max-zoom")?,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Sample a host-bound raster DEM as a ScalarField. `source` names a `dem` entry in the document's `sources` block; omitting it falls back to the document's only `dem` source, with a build warning.",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "Name of a `dem` source in the document's `sources` block. Omitting it resolves to the only such source and warns at build time."
                },
                "min-zoom": { "type": "integer", "minimum": 0, "maximum": 24,
                              "description": "Style-level minimum zoom. Below this zoom the node emits a zero field, as for a tile the source does not have." },
                "max-zoom": { "type": "integer", "minimum": 0, "maximum": 24,
                              "description": "Style-level maximum zoom. Above this zoom the node emits a zero field." }
            },
        })
    }
}

fn resolve_dem_source(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
) -> Result<String, FactoryError> {
    resolve_source(fields, ctx, "`dem`", |decl| {
        matches!(decl, ezu_style::SourceDecl::Dem(_))
    })
}

ezu_graph::submit_node!(DemFactory);
