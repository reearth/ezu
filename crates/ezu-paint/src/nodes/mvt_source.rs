//! `mvt-source` — `() -> Features`. Pulls one MVT layer out of
//! `EvalCtx::tile_data`, applies optional property filter and
//! `min-zoom-field`, and emits the surviving features as a
//! [`FilteredFeatures`](super::common::FilteredFeatures).

use ezu_graph::{
    BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory,
    PortKind, PortSpec, PortValue,
};
use ezu_features::mvt::DecodedTile;
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::common::{features_value, read_optional_string};
use crate::render::{collect_lines, collect_polygons};

struct MvtSourceNode {
    source_layer: String,
    filter: Option<ezu_style::FeatureFilter>,
    min_zoom_field: Option<String>,
}

impl Node for MvtSourceNode {
    fn op_name(&self) -> &'static str {
        "mvt-source"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self) -> PortKind {
        PortKind::Features
    }
    fn coord_space(&self) -> CoordSpace {
        // Features are tile-local (MVT geometry is in [0, extent]).
        CoordSpace::Tile
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        _inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let z = ctx.tile.z;
        let (extent, polygons, lines) = match ctx.tile_data {
            None => (0u32, vec![], vec![]),
            Some(opaque) => {
                let tile = opaque
                    .clone()
                    .downcast::<DecodedTile>()
                    .map_err(|_| EvalError::Other("tile_data is not Arc<DecodedTile>".into()))?;
                match tile.layer(&self.source_layer) {
                    None => (0u32, vec![], vec![]),
                    Some(layer) => {
                        let polys = collect_polygons(
                            &layer.features,
                            &self.filter,
                            &self.min_zoom_field,
                            z,
                        );
                        let lns = collect_lines(
                            &layer.features,
                            &self.filter,
                            &self.min_zoom_field,
                            z,
                        );
                        (layer.extent, polys, lns)
                    }
                }
            }
        };
        Ok(features_value(extent, polygons, lines))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"mvt-source");
        h.update(self.source_layer.as_bytes());
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

pub(super) struct MvtSourceFactory;
impl NodeFactory for MvtSourceFactory {
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let source_layer = fields
            .get("source-layer")
            .and_then(Value::as_str)
            .ok_or_else(|| FactoryError::MissingField("source-layer".into()))?
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
            node: Box::new(MvtSourceNode {
                source_layer,
                filter,
                min_zoom_field,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Select features from a host-supplied MVT layer.",
            "properties": {
                "source-layer": { "type": "string" },
                "filter": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "Property-value filter; entries are AND-combined."
                },
                "min-zoom-field": { "type": "string" },
            },
            "required": ["source-layer"],
        })
    }
}
