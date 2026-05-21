//! GeoJSON parsing.
//!
//! Parses a GeoJSON [`FeatureCollection`], single [`Feature`], or bare
//! geometry into the crate-root [`Feature`] / [`Geometry`] / [`Value`]
//! types. The geometry coordinates are truncated to `i32` so they
//! match MVT's integer tile-local coordinate space — the caller is
//! responsible for any projection or quantization the input needs
//! *before* parsing (e.g. multiplying by an extent or projecting from
//! lon/lat to a pixel grid).

use std::collections::HashMap;

use geojson::{feature::Id as GeoId, GeoJson, Value as GeoVal};

use crate::{Feature, Geometry, Polygon, Value};

#[derive(Debug, thiserror::Error)]
pub enum GeoJsonError {
    // `geojson::Error` is >100 bytes — box it so the `Result` stays small.
    #[error("geojson parse: {0}")]
    Parse(#[from] Box<geojson::Error>),
    #[error("utf-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

impl From<geojson::Error> for GeoJsonError {
    fn from(e: geojson::Error) -> Self {
        GeoJsonError::Parse(Box::new(e))
    }
}

/// Decode a GeoJSON string. Accepts a `Feature`, `FeatureCollection`,
/// or bare `Geometry`; the result is always a flat `Vec<Feature>`
/// (bare geometries become a single feature with no properties).
pub fn decode_str(s: &str) -> Result<Vec<Feature>, GeoJsonError> {
    let parsed: GeoJson = s.parse()?;
    Ok(convert_root(parsed))
}

/// Decode GeoJSON from raw bytes. Convenience wrapper around
/// [`decode_str`] for callers that hold the input as bytes.
pub fn decode(bytes: &[u8]) -> Result<Vec<Feature>, GeoJsonError> {
    decode_str(std::str::from_utf8(bytes)?)
}

fn convert_root(root: GeoJson) -> Vec<Feature> {
    match root {
        GeoJson::FeatureCollection(fc) => fc.features.into_iter().map(convert_feature).collect(),
        GeoJson::Feature(f) => vec![convert_feature(f)],
        GeoJson::Geometry(g) => vec![Feature {
            id: None,
            geometry: convert_geometry(&g.value),
            properties: HashMap::new(),
        }],
    }
}

fn convert_feature(f: geojson::Feature) -> Feature {
    let geometry = f
        .geometry
        .as_ref()
        .map(|g| convert_geometry(&g.value))
        .unwrap_or_default();
    let properties = f
        .properties
        .map(|map| {
            map.into_iter()
                .map(|(k, v)| (k, convert_value(v)))
                .collect()
        })
        .unwrap_or_default();
    Feature {
        id: f.id.and_then(convert_id),
        geometry,
        properties,
    }
}

fn convert_id(id: GeoId) -> Option<u64> {
    match id {
        GeoId::Number(n) => n.as_u64(),
        GeoId::String(_) => None,
    }
}

fn convert_value(v: serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::String(s) => Value::String(s),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(u) = n.as_u64() {
                Value::UInt(u)
            } else if let Some(f) = n.as_f64() {
                Value::Double(f)
            } else {
                Value::Null
            }
        }
        // Arrays / objects don't fit ezu's flat property model.
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => Value::Null,
    }
}

fn convert_geometry(v: &GeoVal) -> Geometry {
    let mut g = Geometry::default();
    accumulate(v, &mut g);
    g
}

fn accumulate(v: &GeoVal, out: &mut Geometry) {
    match v {
        GeoVal::Point(p) => out.points.push(pos_to_xy(p)),
        GeoVal::MultiPoint(ps) => out.points.extend(ps.iter().map(|p| pos_to_xy(p))),
        GeoVal::LineString(line) => {
            out.lines.push(line.iter().map(|p| pos_to_xy(p)).collect());
        }
        GeoVal::MultiLineString(lines) => {
            for l in lines {
                out.lines.push(l.iter().map(|p| pos_to_xy(p)).collect());
            }
        }
        GeoVal::Polygon(rings) => out.polygons.push(ring_set_to_polygon(rings)),
        GeoVal::MultiPolygon(polys) => {
            for p in polys {
                out.polygons.push(ring_set_to_polygon(p));
            }
        }
        GeoVal::GeometryCollection(gs) => {
            for g in gs {
                accumulate(&g.value, out);
            }
        }
    }
}

fn ring_set_to_polygon(rings: &[Vec<Vec<f64>>]) -> Polygon {
    let mut iter = rings.iter().map(|r| ring_to_xy(r));
    let exterior = iter.next().unwrap_or_default();
    let holes = iter.collect();
    Polygon { exterior, holes }
}

/// GeoJSON polygon rings have a duplicated closing vertex (first ==
/// last); MVT and the rest of ezu's pipeline don't, so drop it.
fn ring_to_xy(ring: &[Vec<f64>]) -> Vec<(i32, i32)> {
    let mut out: Vec<(i32, i32)> = ring.iter().map(|p| pos_to_xy(p)).collect();
    if out.len() >= 2 && out.first() == out.last() {
        out.pop();
    }
    out
}

fn pos_to_xy(pos: &[f64]) -> (i32, i32) {
    let x = pos.first().copied().unwrap_or(0.0);
    let y = pos.get(1).copied().unwrap_or(0.0);
    (x.round() as i32, y.round() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
    {
      "type": "FeatureCollection",
      "features": [
        {
          "type": "Feature",
          "id": 42,
          "properties": { "name": "park", "size": 7.5, "active": true },
          "geometry": {
            "type": "Polygon",
            "coordinates": [
              [[0,0], [10,0], [10,10], [0,10], [0,0]],
              [[2,2], [4,2], [4,4], [2,4], [2,2]]
            ]
          }
        },
        {
          "type": "Feature",
          "properties": {},
          "geometry": { "type": "LineString", "coordinates": [[0,0], [5,5], [10,10]] }
        },
        {
          "type": "Feature",
          "properties": {},
          "geometry": { "type": "Point", "coordinates": [3, 4] }
        }
      ]
    }
    "#;

    #[test]
    fn parses_feature_collection() {
        let feats = decode_str(SAMPLE).expect("parse");
        assert_eq!(feats.len(), 3);

        // Polygon with one hole; closing vertex dropped on both rings.
        let poly = &feats[0];
        assert_eq!(poly.id, Some(42));
        assert!(matches!(poly.properties.get("name"), Some(Value::String(s)) if s == "park"));
        assert!(matches!(poly.properties.get("active"), Some(Value::Bool(true))));
        assert_eq!(poly.geometry.polygons.len(), 1);
        assert_eq!(poly.geometry.polygons[0].exterior.len(), 4);
        assert_eq!(poly.geometry.polygons[0].holes.len(), 1);
        assert_eq!(poly.geometry.polygons[0].holes[0].len(), 4);
        assert!(poly.geometry.lines.is_empty());
        assert!(poly.geometry.points.is_empty());

        // LineString → lines vec with one polyline of 3 points.
        assert_eq!(feats[1].geometry.lines.len(), 1);
        assert_eq!(feats[1].geometry.lines[0], vec![(0, 0), (5, 5), (10, 10)]);
        assert!(feats[1].geometry.polygons.is_empty());

        // Point → points vec with one vertex.
        assert_eq!(feats[2].geometry.points, vec![(3, 4)]);
        assert!(feats[2].geometry.lines.is_empty());
    }

    #[test]
    fn parses_single_feature() {
        let s = r#"{"type":"Feature","properties":null,"geometry":{"type":"Point","coordinates":[1,2]}}"#;
        let feats = decode_str(s).unwrap();
        assert_eq!(feats.len(), 1);
        assert_eq!(feats[0].geometry.points, vec![(1, 2)]);
    }

    #[test]
    fn geometry_collection_flattens_into_one_feature() {
        let s = r#"{
          "type": "Feature",
          "properties": { "name": "mixed" },
          "geometry": {
            "type": "GeometryCollection",
            "geometries": [
              { "type": "Point", "coordinates": [1, 2] },
              { "type": "LineString", "coordinates": [[0,0],[3,3]] },
              { "type": "GeometryCollection", "geometries": [
                  { "type": "Point", "coordinates": [9, 9] }
              ]}
            ]
          }
        }"#;
        let feats = decode_str(s).unwrap();
        assert_eq!(feats.len(), 1);
        let g = &feats[0].geometry;
        assert_eq!(g.points, vec![(1, 2), (9, 9)]);
        assert_eq!(g.lines.len(), 1);
        assert!(g.polygons.is_empty());
    }
}
