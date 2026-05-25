//! `dem` — `() -> ScalarField`. Resolves a host-bound DEM mosaic via
//! the unified [`AssetLoader`](ezu_graph::AssetLoader) and emits it as a
//! `ScalarField` port value for `hillshade` / `slope` / `color-ramp`.
//!
//! The host is expected to declare the underlying tile source in the
//! style document's `sources` block, fetch + stitch the tiles, and bind
//! the resulting [`ScalarField`] under `tile.<source-name>` via
//! `TileLoader::bind_scalar_field` before each render. The `name` field
//! on this node is the binding key (typically the same `tile.<source>`
//! string).

use std::sync::Arc;

use ezu_graph::{
    Asset, AssetError, BuiltNode, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue, ScalarField,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

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
                let size = ctx.canvas.padded_size();
                let count = (size * size) as usize;
                return Ok(PortValue::ScalarField(Arc::new(ScalarField {
                    width: size,
                    height: size,
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
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let name = fields
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| FactoryError::MissingField("name".into()))?
            .to_string();
        Ok(BuiltNode {
            node: Box::new(DemNode { name }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Sample a host-bound raster DEM as a ScalarField. `name` is an AssetLoader binding (typically `tile.<source>` matching a `sources` entry).",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Asset binding name. `tile.<source>` for per-tile DEM mosaics bound by the host."
                }
            },
            "required": ["name"],
        })
    }
}

ezu_graph::submit_node!(DemFactory);
