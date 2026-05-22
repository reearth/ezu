//! `features` — `() -> Features`. Resolves a host-bound feature layer
//! via the unified [`AssetLoader`](ezu_graph::AssetLoader), applies an
//! optional property filter and `min-zoom-field`, and emits the
//! surviving features as a [`FilteredFeatures`].
//!
//! Source-format agnostic: the host packs MVT layers, GeoJSON, or any
//! other vector input into [`FeatureLayer`] and binds it by name. Use
//! `tile.<layer>` for per-tile data; bare names for document-scoped
//! bindings. A missing binding is treated as "no features for this
//! tile" and yields an empty result.

use ezu_graph::{
    Asset, AssetError, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, Node,
    NodeFactory, PortKind, PortSpec, PortValue,
};
use ezu_features::FeatureLayer;
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{features_value, read_optional_string};
use crate::render::{collect_lines, collect_points, collect_polygons};

struct FeaturesNode {
    name: String,
    filter: Option<ezu_style::FeatureFilter>,
    min_zoom_field: Option<String>,
}

impl Node for FeaturesNode {
    fn op_name(&self) -> &'static str {
        "features"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self) -> PortKind {
        PortKind::Features
    }
    fn coord_space(&self) -> CoordSpace {
        // Features live in tile-local coordinates ([0, extent]).
        CoordSpace::Tile
    }
    fn asset_inputs(&self) -> Vec<String> {
        vec![self.name.clone()]
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        _inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let z = ctx.tile.z;
        let asset = match ctx.assets.load(&self.name) {
            Ok(a) => a,
            // No binding for this tile -> emit an empty layer.
            Err(AssetError::NotFound(_)) => return Ok(features_value(0, vec![], vec![], vec![])),
            Err(e) => return Err(EvalError::Asset(e)),
        };
        let Asset::Features(opq) = asset else {
            return Err(EvalError::Other(format!(
                "asset `{}` is not a feature layer",
                self.name
            )));
        };
        let layer = opq
            .downcast::<FeatureLayer>()
            .map_err(|_| EvalError::Other(format!("`{}` payload is not FeatureLayer", self.name)))?;
        let polys = collect_polygons(&layer.features, &self.filter, &self.min_zoom_field, z);
        let lns = collect_lines(&layer.features, &self.filter, &self.min_zoom_field, z);
        let pts = collect_points(&layer.features, &self.filter, &self.min_zoom_field, z);
        Ok(features_value(layer.extent, polys, lns, pts))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"features");
        h.update(self.name.as_bytes());
        if let Some(f) = &self.filter {
            let mut keys: Vec<&String> = f.keys().collect();
            keys.sort();
            for k in keys {
                h.update(k.as_bytes());
                // Lightweight hash of the FilterMatch via Debug; not
                // beautiful but stable enough for cache invalidation.
                h.update(format!("{:?}", f[k]).as_bytes());
            }
        }
        if let Some(s) = &self.min_zoom_field {
            h.update(s.as_bytes());
        }
    }
}

pub(super) struct FeaturesFactory;
impl NodeFactory for FeaturesFactory {
    fn op_name(&self) -> &'static str {
        "features"
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
        let filter = match fields.get("filter") {
            Some(v) => Some(
                serde_json::from_value::<ezu_style::FeatureFilter>(v.clone()).map_err(|e| {
                    FactoryError::BadField {
                        field: "filter".into(),
                        msg: e.to_string(),
                    }
                })?,
            ),
            None => None,
        };
        let min_zoom_field = read_optional_string(fields, "min-zoom-field")?;
        Ok(BuiltNode {
            node: Box::new(FeaturesNode {
                name,
                filter,
                min_zoom_field,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Sample features from a host-bound layer. `name` is an AssetLoader binding (e.g. `tile.buildings`); use `tile.*` for per-tile data, bare names for document-scoped.",
            "properties": {
                "name": { "type": "string",
                          "description": "Asset binding name. `tile.<layer>` for per-tile features (MVT, GeoJSON, …) bound by the host." },
                "filter": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "Property-value filter; entries are AND-combined."
                },
                "min-zoom-field": { "type": "string" },
            },
            "required": ["name"],
        })
    }
}

ezu_graph::submit_node!(FeaturesFactory);
