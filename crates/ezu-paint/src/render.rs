//! Feature filtering and collection helpers shared by MVT-driven
//! nodes (`mvt-source`).

use ezu_features::{Feature, Geometry, Polygon, Value};
use ezu_style::{FeatureFilter, FilterAtom, FilterMatch};

/// Walk a layer's features and return every polygon ring that passes
/// the filter. Polygons are cloned out so callers own contiguous data.
pub fn collect_polygons(
    features: &[Feature],
    filter: &Option<FeatureFilter>,
    min_zoom_field: &Option<String>,
    z: u8,
) -> Vec<Polygon> {
    let mut out = Vec::new();
    for f in features {
        if !feature_passes(f, filter, min_zoom_field, z) {
            continue;
        }
        if let Geometry::Polygons(ps) = &f.geometry {
            for p in ps {
                out.push(Polygon {
                    exterior: p.exterior.clone(),
                    holes: p.holes.clone(),
                });
            }
        }
    }
    out
}

/// Walk a layer's features and return every polyline that passes the
/// filter.
pub fn collect_lines(
    features: &[Feature],
    filter: &Option<FeatureFilter>,
    min_zoom_field: &Option<String>,
    z: u8,
) -> Vec<Vec<(i32, i32)>> {
    let mut out = Vec::new();
    for f in features {
        if !feature_passes(f, filter, min_zoom_field, z) {
            continue;
        }
        if let Geometry::Lines(ls) = &f.geometry {
            out.extend(ls.iter().cloned());
        }
    }
    out
}

fn feature_passes(
    f: &Feature,
    filter: &Option<FeatureFilter>,
    min_zoom_field: &Option<String>,
    z: u8,
) -> bool {
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
            let Some(actual) = f.properties.get(k) else {
                return false;
            };
            if !match_value(actual, expected) {
                return false;
            }
        }
    }
    true
}

fn match_value(actual: &Value, expected: &FilterMatch) -> bool {
    match expected {
        FilterMatch::One(atom) => atom_equals(actual, atom),
        FilterMatch::Any(atoms) => atoms.iter().any(|a| atom_equals(actual, a)),
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
