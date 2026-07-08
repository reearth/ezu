//! Feature filtering and collection helpers shared by feature-source
//! nodes (`features`).

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use ezu_features::{Feature, FeatureLayer, Value};
use maplibre_expr::{EvaluationContext, Expr, Feature as ExprFeature, Value as ExprValue};

use crate::nodes::common::FeatureGroup;

/// A host-bound feature layer paired with a lazily-built, node-shared
/// conversion of its features into the `maplibre-expr` value form.
///
/// A layer is bound once per (tile, source, layer); every `features` node that
/// references the same `<source>.<layer>` receives the *same*
/// `Arc<SharedLayer>`. Converting each feature's properties and building its
/// filter [`EvaluationContext`] therefore happens once per tile — behind a
/// [`OnceLock`] — instead of once per referencing node. A Protomaps basemap
/// aims dozens of `features` nodes at the single `roads` layer (each with its
/// own filter), so sharing collapses that many redundant conversions to one.
pub struct SharedLayer {
    pub layer: FeatureLayer,
    prepared: OnceLock<PreparedLayer>,
}

impl SharedLayer {
    pub fn new(layer: FeatureLayer) -> SharedLayer {
        SharedLayer {
            layer,
            prepared: OnceLock::new(),
        }
    }

    /// The per-feature converted view, built on first use at tile zoom `z`.
    /// A `SharedLayer` is bound per tile, so `z` is constant across the calls
    /// that reach one instance.
    fn prepared(&self, z: u8) -> &PreparedLayer {
        self.prepared
            .get_or_init(|| PreparedLayer::build(&self.layer, z))
    }
}

/// Per-feature conversions shared across every node that reads a layer: the
/// properties in `maplibre-expr` value form (behind an `Arc`, reused directly
/// as a surviving group's `properties`) and a ready-to-borrow filter
/// [`EvaluationContext`]. Indexed parallel to [`SharedLayer::layer`]'s
/// `features`.
struct PreparedLayer {
    features: Vec<PreparedFeature>,
}

struct PreparedFeature {
    props: Arc<BTreeMap<String, ExprValue>>,
    ctx: EvaluationContext,
}

impl PreparedLayer {
    fn build(layer: &FeatureLayer, z: u8) -> PreparedLayer {
        let features = layer
            .features
            .iter()
            .map(|f| {
                let props: BTreeMap<String, ExprValue> = f
                    .properties
                    .iter()
                    .map(|(k, v)| (k.clone(), value_to_expr(v)))
                    .collect();
                let ctx = ctx_from_props(&props, geometry_type(f), z);
                PreparedFeature {
                    props: Arc::new(props),
                    ctx,
                }
            })
            .collect();
        PreparedLayer { features }
    }
}

/// Walk a layer's features and return one [`FeatureGroup`] per surviving
/// feature, preserving its properties alongside its own geometry. This is the
/// only representation of a `Features` payload; consumers that want the flat
/// geometry view walk the groups. Each feature's properties are converted into
/// the `maplibre-expr` value form once per tile (see [`SharedLayer`]) and
/// shared via `Arc`, so both the per-feature filter here and downstream
/// data-driven paint pay no re-conversion cost. Features that contribute no
/// geometry at all are skipped (they'd paint nothing).
pub fn collect_groups(
    shared: &SharedLayer,
    filter_expr: Option<&Expr>,
    min_zoom_field: &Option<String>,
    z: u8,
) -> Vec<FeatureGroup> {
    let prepared = shared.prepared(z);
    let mut out = Vec::new();
    for (f, pf) in shared.layer.features.iter().zip(&prepared.features) {
        // A MapLibre filter expression (full expression language: `any`,
        // `has`, comparisons, `geometry-type`, …), evaluated per feature
        // against the shared context. A feature passes only if the expression
        // is truthy; an eval error excludes it.
        if let Some(expr) = filter_expr {
            let ok = maplibre_expr::evaluate(expr, &pf.ctx)
                .map(|v| v.is_truthy())
                .unwrap_or(false);
            if !ok {
                continue;
            }
        }
        if let Some(field) = min_zoom_field.as_ref() {
            let ok = f
                .properties
                .get(field)
                .and_then(value_as_i64)
                .map(|mz| mz <= z as i64)
                .unwrap_or(true); // missing field → assume visible
            if !ok {
                continue;
            }
        }
        if f.geometry.polygons.is_empty()
            && f.geometry.lines.is_empty()
            && f.geometry.points.is_empty()
        {
            continue;
        }
        out.push(FeatureGroup {
            properties: pf.props.clone(),
            polygons: f.geometry.polygons.clone(),
            lines: f.geometry.lines.clone(),
            points: f.geometry.points.clone(),
        });
    }
    out
}

/// Build a maplibre-expr evaluation context from already-converted properties,
/// a geometry-type string, and the tile zoom.
fn ctx_from_props(
    props: &BTreeMap<String, ExprValue>,
    geometry_type: &str,
    z: u8,
) -> EvaluationContext {
    EvaluationContext::new()
        .with_zoom(z as f64)
        .with_feature(ExprFeature {
            properties: props.clone(),
            geometry_type: Some(geometry_type.to_string()),
            ..Default::default()
        })
}

/// Build a maplibre-expr evaluation context for a [`FeatureGroup`]: its
/// properties, geometry type (highest dimension present), and the tile zoom.
/// The group's own `properties`/geometry drive a per-feature paint expression.
///
/// The group's properties are already in `maplibre-expr` value form (converted
/// once in [`collect_groups`]); the clone here is bounded by the maplibre-expr
/// API — `ExprFeature` owns its `properties` map — so it is one owned map per
/// group per node evaluation, not a re-conversion.
pub(crate) fn group_expr_context(
    g: &crate::nodes::common::FeatureGroup,
    z: u8,
) -> EvaluationContext {
    let geometry_type = if !g.polygons.is_empty() {
        "Polygon"
    } else if !g.lines.is_empty() {
        "LineString"
    } else {
        "Point"
    };
    EvaluationContext::new()
        .with_zoom(z as f64)
        .with_feature(ExprFeature {
            properties: (*g.properties).clone(),
            geometry_type: Some(geometry_type.to_string()),
            ..Default::default()
        })
}

/// The MapLibre `geometry-type` string for a feature. MVT features carry
/// one geometry class; if several are present, report the highest-dimension
/// one (polygon > line > point).
pub(crate) fn geometry_type(f: &Feature) -> &'static str {
    if !f.geometry.polygons.is_empty() {
        "Polygon"
    } else if !f.geometry.lines.is_empty() {
        "LineString"
    } else {
        "Point"
    }
}

/// Convert an ezu feature-property value into a maplibre-expr value. All
/// numeric kinds collapse to `Number`; there is no distinct integer type in
/// the expression language.
pub(crate) fn value_to_expr(v: &Value) -> ExprValue {
    match v {
        Value::String(s) => ExprValue::String(s.clone()),
        Value::Bool(b) => ExprValue::Bool(*b),
        Value::Float(n) => ExprValue::Number(*n as f64),
        Value::Double(n) => ExprValue::Number(*n),
        Value::Int(n) | Value::SInt(n) => ExprValue::Number(*n as f64),
        Value::UInt(n) => ExprValue::Number(*n as f64),
        Value::Null => ExprValue::Null,
    }
}

fn value_as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Int(n) | Value::SInt(n) => Some(*n),
        Value::UInt(n) => Some(*n as i64),
        Value::Float(n) => Some(*n as i64),
        Value::Double(n) => Some(*n as i64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ezu_features::{Geometry, Polygon};
    use std::collections::HashMap;

    fn feat(props: &[(&str, Value)]) -> Feature {
        let mut properties = HashMap::new();
        for (k, v) in props {
            properties.insert(k.to_string(), v.clone());
        }
        let mut geometry = Geometry::default();
        geometry.polygons.push(Polygon {
            exterior: vec![(0, 0), (10, 0), (10, 10)],
            holes: vec![],
        });
        Feature {
            id: None,
            geometry,
            properties,
        }
    }

    fn expr(j: serde_json::Value) -> Expr {
        maplibre_expr::parse(&j).unwrap()
    }

    fn passes(f: &Feature, e: &Expr) -> bool {
        let layer = ezu_features::FeatureLayer {
            name: "t".into(),
            extent: 4096,
            features: vec![f.clone()],
        };
        let shared = SharedLayer::new(layer);
        !collect_groups(&shared, Some(e), &None, 14).is_empty()
    }

    #[test]
    fn expr_filter_comparison_and_has() {
        // Operators the structured filter can't express: `>` and `has`.
        let e = expr(serde_json::json!([
            "all",
            [">", ["get", "area"], 10],
            ["has", "name"]
        ]));
        assert!(passes(
            &feat(&[
                ("area", Value::Int(50)),
                ("name", Value::String("x".into()))
            ]),
            &e
        ));
        assert!(!passes(
            &feat(&[("area", Value::Int(5)), ("name", Value::String("x".into()))]),
            &e
        )); // area too small
        assert!(!passes(&feat(&[("area", Value::Int(50))]), &e)); // no `name`
    }

    #[test]
    fn expr_filter_any_and_geometry_type() {
        let e = expr(serde_json::json!([
            "all",
            [
                "any",
                ["==", ["get", "class"], "a"],
                ["==", ["get", "class"], "b"]
            ],
            ["==", ["geometry-type"], "Polygon"]
        ]));
        assert!(passes(&feat(&[("class", Value::String("a".into()))]), &e));
        assert!(passes(&feat(&[("class", Value::String("b".into()))]), &e));
        assert!(!passes(&feat(&[("class", Value::String("c".into()))]), &e));
    }

    #[test]
    fn expr_filter_all_combines_clauses() {
        // `all` AND-combines an equality and a comparison; a feature must
        // satisfy every sub-expression to pass.
        let e = expr(serde_json::json!([
            "all",
            ["==", ["get", "class"], "road"],
            [">=", ["get", "lanes"], 2]
        ]));
        assert!(passes(
            &feat(&[
                ("class", Value::String("road".into())),
                ("lanes", Value::Int(4)),
            ]),
            &e
        ));
        assert!(!passes(
            &feat(&[
                ("class", Value::String("path".into())),
                ("lanes", Value::Int(4)),
            ]),
            &e
        )); // wrong class
        assert!(!passes(
            &feat(&[
                ("class", Value::String("road".into())),
                ("lanes", Value::Int(1)),
            ]),
            &e
        )); // too few lanes
    }
}
