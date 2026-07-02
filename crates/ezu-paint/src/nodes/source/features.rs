//! `features` — `() -> Features`. Resolves a host-bound feature layer
//! via the unified [`AssetLoader`](ezu_graph::AssetLoader), applies an
//! optional property filter and `min-zoom-field`, and emits the
//! surviving features as a [`FilteredFeatures`].
//!
//! Style fields: `source` (optional, matches a `mvt`/`pmtiles` entry
//! in the document's `sources` block; defaults to the single such
//! entry when only one exists) + `layer` (the MVT layer name). The
//! op looks up `<source>.<layer>` on the host's AssetLoader.
//! A missing binding is treated as "no features for this tile" and
//! yields an empty result.

use ezu_features::FeatureLayer;
use ezu_graph::{
    Asset, AssetError, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, Node,
    NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{features_value, read_optional_string};
use crate::render::{collect_lines, collect_points, collect_polygons};

struct FeaturesNode {
    name: String,
    filter: Option<ezu_style::FeatureFilter>,
    /// A MapLibre-expression filter, compiled once. Evaluated per feature
    /// (AND-combined with the structured `filter` when both are present).
    filter_expr: Option<maplibre_expr::Expr>,
    /// The raw `filter-expr` JSON text, kept only for a stable cache hash.
    filter_expr_src: Option<String>,
    min_zoom_field: Option<String>,
    min_zoom: Option<u8>,
    max_zoom: Option<u8>,
}

impl Node for FeaturesNode {
    fn op_name(&self) -> &'static str {
        "features"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
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
        // Style-level zoom gate: outside the [min_zoom, max_zoom] band,
        // skip the asset lookup entirely and emit an empty layer.
        if self.min_zoom.is_some_and(|mn| z < mn) || self.max_zoom.is_some_and(|mx| z > mx) {
            return Ok(features_value(0, vec![], vec![], vec![]));
        }
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
        let layer = opq.downcast::<FeatureLayer>().map_err(|_| {
            EvalError::Other(format!("`{}` payload is not FeatureLayer", self.name))
        })?;
        let fe = self.filter_expr.as_ref();
        let polys = collect_polygons(&layer.features, &self.filter, fe, &self.min_zoom_field, z);
        let lns = collect_lines(&layer.features, &self.filter, fe, &self.min_zoom_field, z);
        let pts = collect_points(&layer.features, &self.filter, fe, &self.min_zoom_field, z);
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
        if let Some(s) = &self.filter_expr_src {
            h.update(b"fexpr");
            h.update(s.as_bytes());
        }
        if let Some(s) = &self.min_zoom_field {
            h.update(s.as_bytes());
        }
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

pub(super) struct FeaturesFactory;
impl NodeFactory for FeaturesFactory {
    fn op_name(&self) -> &'static str {
        "features"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let layer = fields
            .get("layer")
            .and_then(Value::as_str)
            .ok_or_else(|| FactoryError::MissingField("layer".into()))?
            .to_string();
        let source = resolve_feature_source(fields, ctx)?;
        let name = format!("{source}.{layer}");
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
        // `filter-expr`: a raw MapLibre filter expression, compiled once.
        let (filter_expr, filter_expr_src) = match fields.get("filter-expr") {
            Some(v) => {
                let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
                    field: "filter-expr".into(),
                    msg: e.to_string(),
                })?;
                (Some(expr), Some(v.to_string()))
            }
            None => (None, None),
        };
        let min_zoom_field = read_optional_string(fields, "min-zoom-field")?;
        let min_zoom = read_optional_zoom(fields, "min-zoom")?;
        let max_zoom = read_optional_zoom(fields, "max-zoom")?;
        Ok(BuiltNode {
            node: Box::new(FeaturesNode {
                name,
                filter,
                filter_expr,
                filter_expr_src,
                min_zoom_field,
                min_zoom,
                max_zoom,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Sample features from a host-bound vector tile layer. `source` names a `mvt`/`pmtiles` entry in the document's `sources` block (optional when exactly one exists); `layer` selects a layer within that source.",
            "properties": {
                "source": { "type": "string",
                            "description": "Name of an `mvt` or `pmtiles` entry in the document's `sources`. Optional — defaults to the only such source when the document declares exactly one." },
                "layer": { "type": "string",
                           "description": "Vector tile layer name within `source` (e.g. `earth`, `roads`)." },
                "filter": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "Property-value filter; entries are AND-combined."
                },
                "filter-expr": {
                    "description": "A MapLibre filter expression (JSON array, e.g. [\"all\", [\"==\", [\"get\", \"class\"], \"primary\"], [\"has\", \"name\"]]), evaluated per feature. AND-combined with `filter` if both are given. Supports the full expression language (any/has/comparisons/geometry-type)."
                },
                "min-zoom-field": { "type": "string",
                                    "description": "Per-feature property name carrying its data-side `min_zoom`. Features with `<field> > z` are dropped." },
                "min-zoom": { "type": "integer", "minimum": 0, "maximum": 24,
                              "description": "Style-level minimum zoom. Below this zoom the node emits an empty layer (the asset is not even loaded)." },
                "max-zoom": { "type": "integer", "minimum": 0, "maximum": 24,
                              "description": "Style-level maximum zoom. Above this zoom the node emits an empty layer." },
            },
            "required": ["layer"],
        })
    }
}

/// Resolve the `source` field for ops that target a vector tile
/// source. When omitted, defaults to the document's single
/// `mvt`/`pmtiles` source. Errors if `source` is omitted and the
/// document has zero or multiple such sources, or if a named source
/// doesn't exist / isn't a vector tile source.
fn resolve_feature_source(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
) -> Result<String, FactoryError> {
    if let Some(name) = read_optional_string(fields, "source")? {
        match ctx.sources.get(&name) {
            Some(ezu_style::SourceDecl::Mvt(_)) | Some(ezu_style::SourceDecl::Pmtiles(_)) => {
                Ok(name)
            }
            Some(_) => Err(FactoryError::BadField {
                field: "source".into(),
                msg: format!("`{name}` exists but is not an `mvt` / `pmtiles` source"),
            }),
            None => Err(FactoryError::BadField {
                field: "source".into(),
                msg: format!("no source named `{name}` in document"),
            }),
        }
    } else {
        let mut matches = ctx.sources.iter().filter(|(_, decl)| {
            matches!(
                decl,
                ezu_style::SourceDecl::Mvt(_) | ezu_style::SourceDecl::Pmtiles(_)
            )
        });
        match (matches.next(), matches.next()) {
            (Some((name, _)), None) => Ok(name.clone()),
            (None, _) => Err(FactoryError::BadField {
                field: "source".into(),
                msg:
                    "no `mvt`/`pmtiles` source in document; declare one or pass `source` explicitly"
                        .into(),
            }),
            (Some(_), Some(_)) => Err(FactoryError::BadField {
                field: "source".into(),
                msg: "multiple `mvt`/`pmtiles` sources in document; pass `source` explicitly"
                    .into(),
            }),
        }
    }
}

fn read_optional_zoom(
    fields: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<u8>, FactoryError> {
    let Some(v) = fields.get(key) else {
        return Ok(None);
    };
    let n = v.as_u64().ok_or_else(|| FactoryError::BadField {
        field: key.into(),
        msg: "expected non-negative integer".into(),
    })?;
    if n > 24 {
        return Err(FactoryError::BadField {
            field: key.into(),
            msg: format!("zoom {n} out of range (0..=24)"),
        });
    }
    Ok(Some(n as u8))
}

ezu_graph::submit_node!(FeaturesFactory);
