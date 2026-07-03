//! Feature filtering and collection helpers shared by feature-source
//! nodes (`features`).

use std::collections::BTreeMap;

use ezu_features::{Feature, Polygon, Value};
use ezu_style::{FeatureFilter, FilterAtom, FilterMatch, NotInner};
use maplibre_expr::{EvaluationContext, Expr, Feature as ExprFeature, Value as ExprValue};

/// Walk a layer's features and return every polygon ring that passes
/// the filter. Polygons are cloned out so callers own contiguous data.
pub fn collect_polygons(
    features: &[Feature],
    filter: &Option<FeatureFilter>,
    filter_expr: Option<&Expr>,
    min_zoom_field: &Option<String>,
    z: u8,
) -> Vec<Polygon> {
    let mut out = Vec::new();
    for f in features {
        if !feature_passes(f, filter, filter_expr, min_zoom_field, z) {
            continue;
        }
        out.extend(f.geometry.polygons.iter().cloned());
    }
    out
}

/// Walk a layer's features and return every point that passes the
/// filter.
pub fn collect_points(
    features: &[Feature],
    filter: &Option<FeatureFilter>,
    filter_expr: Option<&Expr>,
    min_zoom_field: &Option<String>,
    z: u8,
) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    for f in features {
        if !feature_passes(f, filter, filter_expr, min_zoom_field, z) {
            continue;
        }
        out.extend(f.geometry.points.iter().copied());
    }
    out
}

/// Walk a layer's features and return every polyline that passes the
/// filter.
pub fn collect_lines(
    features: &[Feature],
    filter: &Option<FeatureFilter>,
    filter_expr: Option<&Expr>,
    min_zoom_field: &Option<String>,
    z: u8,
) -> Vec<Vec<(i32, i32)>> {
    let mut out = Vec::new();
    for f in features {
        if !feature_passes(f, filter, filter_expr, min_zoom_field, z) {
            continue;
        }
        out.extend(f.geometry.lines.iter().cloned());
    }
    out
}

/// Walk a layer's features and return one [`FeatureGroup`] per surviving
/// feature, preserving its properties alongside its own geometry. Used by
/// data-driven paint, which evaluates a per-feature expression. Features
/// that contribute no geometry at all are skipped (they'd paint nothing).
pub fn collect_groups(
    features: &[Feature],
    filter: &Option<FeatureFilter>,
    filter_expr: Option<&Expr>,
    min_zoom_field: &Option<String>,
    z: u8,
) -> Vec<crate::nodes::common::FeatureGroup> {
    let mut out = Vec::new();
    for f in features {
        if !feature_passes(f, filter, filter_expr, min_zoom_field, z) {
            continue;
        }
        if f.geometry.polygons.is_empty()
            && f.geometry.lines.is_empty()
            && f.geometry.points.is_empty()
        {
            continue;
        }
        out.push(crate::nodes::common::FeatureGroup {
            properties: f.properties.clone(),
            polygons: f.geometry.polygons.clone(),
            lines: f.geometry.lines.clone(),
            points: f.geometry.points.clone(),
        });
    }
    out
}

fn feature_passes(
    f: &Feature,
    filter: &Option<FeatureFilter>,
    filter_expr: Option<&Expr>,
    min_zoom_field: &Option<String>,
    z: u8,
) -> bool {
    // A MapLibre-expression filter (full expression language: `any`, `has`,
    // comparisons, `geometry-type`, …), evaluated per feature. A feature
    // passes only if the expression is truthy; an eval error excludes it.
    if let Some(expr) = filter_expr {
        let ctx = expr_context(f, z);
        let ok = maplibre_expr::evaluate(expr, &ctx)
            .map(|v| v.is_truthy())
            .unwrap_or(false);
        if !ok {
            return false;
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
            return false;
        }
    }
    if let Some(filter) = filter.as_ref() {
        for (k, expected) in filter {
            match f.properties.get(k) {
                Some(actual) => {
                    if !match_value(actual, expected) {
                        return false;
                    }
                }
                // Missing property: passes a Not clause (the value is
                // not in the excluded set, vacuously), fails One/Any.
                None => {
                    if !matches!(expected, FilterMatch::Not(_)) {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// Build a maplibre-expr evaluation context for one feature: its
/// properties, geometry type, and the tile zoom.
pub(crate) fn expr_context(f: &Feature, z: u8) -> EvaluationContext {
    let properties: BTreeMap<String, ExprValue> = f
        .properties
        .iter()
        .map(|(k, v)| (k.clone(), value_to_expr(v)))
        .collect();
    EvaluationContext::new()
        .with_zoom(z as f64)
        .with_feature(ExprFeature {
            properties,
            geometry_type: Some(geometry_type(f).to_string()),
            ..Default::default()
        })
}

/// Build a maplibre-expr evaluation context for a [`FeatureGroup`]: its
/// properties, geometry type (highest dimension present), and the tile zoom.
/// The group's own `properties`/geometry drive a per-feature paint expression.
pub(crate) fn group_expr_context(
    g: &crate::nodes::common::FeatureGroup,
    z: u8,
) -> EvaluationContext {
    let properties: BTreeMap<String, ExprValue> = g
        .properties
        .iter()
        .map(|(k, v)| (k.clone(), value_to_expr(v)))
        .collect();
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
            properties,
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

fn match_value(actual: &Value, expected: &FilterMatch) -> bool {
    match expected {
        FilterMatch::One(atom) => atom_equals(actual, atom),
        FilterMatch::Any(atoms) => atoms.iter().any(|a| atom_equals(actual, a)),
        FilterMatch::Not(n) => match &n.not {
            NotInner::One(atom) => !atom_equals(actual, atom),
            NotInner::Any(atoms) => !atoms.iter().any(|a| atom_equals(actual, a)),
        },
    }
}

fn atom_equals(actual: &Value, expected: &FilterAtom) -> bool {
    match (actual, expected) {
        (Value::String(a), FilterAtom::Str(b)) => a == b,
        (Value::Bool(a), FilterAtom::Bool(b)) => a == b,
        (Value::Int(a), FilterAtom::Int(b)) | (Value::SInt(a), FilterAtom::Int(b)) => a == b,
        (Value::UInt(a), FilterAtom::Int(b)) => (*a as i64) == *b,
        (Value::Float(a), FilterAtom::Float(b)) => (*a as f64) == *b,
        (Value::Double(a), FilterAtom::Float(b)) => a == b,
        (Value::Int(a), FilterAtom::Float(b)) | (Value::SInt(a), FilterAtom::Float(b)) => {
            (*a as f64) == *b
        }
        _ => false,
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
        feature_passes(f, &None, Some(e), &None, 14)
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
    fn expr_filter_and_structured_filter_are_anded() {
        // Structured filter (class == road) AND expr (lanes >= 2).
        use ezu_style::{FeatureFilter, FilterAtom, FilterMatch};
        let mut sf: FeatureFilter = FeatureFilter::new();
        sf.insert(
            "class".to_string(),
            FilterMatch::One(FilterAtom::Str("road".into())),
        );
        let e = expr(serde_json::json!([">=", ["get", "lanes"], 2]));
        let f_ok = feat(&[
            ("class", Value::String("road".into())),
            ("lanes", Value::Int(4)),
        ]);
        let f_wrong_class = feat(&[
            ("class", Value::String("path".into())),
            ("lanes", Value::Int(4)),
        ]);
        let f_few_lanes = feat(&[
            ("class", Value::String("road".into())),
            ("lanes", Value::Int(1)),
        ]);
        assert!(feature_passes(
            &f_ok,
            &Some(sf.clone()),
            Some(&e),
            &None,
            14
        ));
        assert!(!feature_passes(
            &f_wrong_class,
            &Some(sf.clone()),
            Some(&e),
            &None,
            14
        ));
        assert!(!feature_passes(
            &f_few_lanes,
            &Some(sf),
            Some(&e),
            &None,
            14
        ));
    }
}
