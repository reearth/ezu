//! `segment-to` — `Features -> Features`. Turns each input point into a
//! two-vertex polyline running from a position its feature names by
//! longitude and latitude to the point itself: a leader from a label
//! anchor to the thing it labels, or a connector from a building's
//! representative point to its entrance.
//!
//! The other end comes from two properties holding WGS84 degrees, named
//! by `lng-field` and `lat-field`, projected into the tile's frame from
//! the tile being rendered. Each line keeps its point's properties. A
//! feature whose fields are missing or not coordinates, or whose two ends
//! coincide, yields nothing.
//!
//! A connector reaches into a tile from points outside it, which a tile's
//! thin buffer may not carry; `stitch: "pieces"` on the upstream
//! `features` node brings in the neighbouring tiles' points too.

use ezu_core::coord::{lat_to_world_y, lon_to_world_x};
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, InfluenceCtx, Node, NodeFactory, PortKind, PortSpec, PortValue, TileId,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{downcast_features, features_value, read_optional_string, FeatureGroup};

/// Furthest from the tile origin, in extent units, an end may land.
/// Anything further is no connector worth drawing, and staying well inside
/// `i32` keeps the arithmetic downstream from overflowing.
const MAX_COORD: f64 = (i32::MAX / 4) as f64;

struct SegmentToNode {
    lng_field: String,
    lat_field: String,
}

impl Node for SegmentToNode {
    fn op_name(&self) -> &'static str {
        "segment-to"
    }
    fn inputs(&self) -> &[PortSpec] {
        static SPECS: &[PortSpec] = &[PortSpec {
            name: "features",
            accepts: &[PortKind::Features],
            optional: false,
        }];
        SPECS
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Features
    }
    fn coord_space(&self) -> CoordSpace {
        CoordSpace::Tile
    }
    fn influence_pad(&self, _ctx: &InfluenceCtx<'_>) -> u32 {
        // A point any distance away can send its connector across the tile.
        InfluenceCtx::UNBOUNDED
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let feats = downcast_features(
            inputs[0]
                .as_ref()
                .ok_or_else(|| EvalError::MissingInput("features".into()))?,
        )?;
        let mut out_groups = Vec::new();
        for g in &feats.groups {
            let Some(target) = target_of(g, &self.lng_field, &self.lat_field)
                .and_then(|(lng, lat)| project(ctx.tile, feats.extent, lng, lat))
            else {
                continue;
            };
            let lines: Vec<Vec<(i32, i32)>> = g
                .points
                .iter()
                .filter(|&&p| p != target)
                .map(|&p| vec![target, p])
                .collect();
            if lines.is_empty() {
                continue;
            }
            out_groups.push(FeatureGroup {
                properties: g.properties.clone(),
                polygons: vec![],
                lines,
                points: vec![],
            });
        }
        Ok(features_value(feats.extent, out_groups))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"segment-to");
        h.update(self.lng_field.as_bytes());
        h.update(&[0]);
        h.update(self.lat_field.as_bytes());
    }
}

/// The longitude and latitude a feature's properties give, if both are
/// numbers (or numeric strings) within range.
fn target_of(g: &FeatureGroup, lng_field: &str, lat_field: &str) -> Option<(f64, f64)> {
    let number = |field: &str| match g.properties.get(field)? {
        maplibre_expr::Value::Number(n) => Some(*n),
        maplibre_expr::Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    let lng = number(lng_field).filter(|v| (-180.0..=180.0).contains(v))?;
    let lat = number(lat_field).filter(|v| (-90.0..=90.0).contains(v))?;
    Some((lng, lat))
}

/// `lng` / `lat` in the frame of `tile` at `extent` units a side.
fn project(tile: TileId, extent: u32, lng: f64, lat: f64) -> Option<(i32, i32)> {
    let n = 2f64.powi(i32::from(tile.z));
    let e = f64::from(extent);
    let x = (lon_to_world_x(lng) * n - f64::from(tile.x)) * e;
    let y = (lat_to_world_y(lat) * n - f64::from(tile.y)) * e;
    if x.abs() > MAX_COORD || y.abs() > MAX_COORD {
        return None;
    }
    Some((x.round() as i32, y.round() as i32))
}

pub(super) struct SegmentToFactory;
impl NodeFactory for SegmentToFactory {
    fn op_name(&self) -> &'static str {
        "segment-to"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let lng_field = read_optional_string(fields, "lng-field")?
            .ok_or_else(|| FactoryError::MissingField("lng-field".into()))?;
        let lat_field = read_optional_string(fields, "lat-field")?
            .ok_or_else(|| FactoryError::MissingField("lat-field".into()))?;
        Ok(BuiltNode {
            node: Box::new(SegmentToNode {
                lng_field,
                lat_field,
            }),
            connections: vec![Connection {
                port: "features".into(),
                src: features,
            }],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Turn each point into a two-vertex polyline from a position its feature's properties give in longitude and latitude to the point itself: a leader from a label anchor to what it labels, a connector from a building's representative point to its entrance. Each line keeps its point's properties; dash it with `dash` and paint it with `stroke`. Lines and polygons are ignored, as are features whose fields are missing, not numbers (or numeric strings), or out of range, and points that coincide with their other end. A connector can cross a tile from a point outside it, beyond the tile's thin buffer; set `stitch: \"pieces\"` on the upstream `features` node to bring in the neighbouring tiles' points too.",
            "properties": {
                "features": schema_frag::node_ref(),
                "lng-field": { "type": "string",
                    "description": "Property holding the other end's longitude, in WGS84 degrees." },
                "lat-field": { "type": "string",
                    "description": "Property holding the other end's latitude, in WGS84 degrees." },
            },
            "required": ["features", "lng-field", "lat-field"],
        })
    }
}

ezu_graph::submit_node!(SegmentToFactory);

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use ezu_core::coord::{world_x_to_lon, world_y_to_lat};

    use super::*;

    const TILE: TileId = TileId {
        z: 16,
        x: 58_210,
        y: 25_803,
    };
    const EXTENT: u32 = 4096;

    /// The longitude and latitude of `(x, y)` in [`TILE`]'s frame.
    fn lng_lat(x: f64, y: f64) -> (f64, f64) {
        let n = f64::from(1u32 << TILE.z);
        let e = f64::from(EXTENT);
        (
            world_x_to_lon((f64::from(TILE.x) + x / e) / n),
            world_y_to_lat((f64::from(TILE.y) + y / e) / n),
        )
    }

    fn group(props: &[(&str, maplibre_expr::Value)], points: Vec<(i32, i32)>) -> FeatureGroup {
        let properties: BTreeMap<String, maplibre_expr::Value> = props
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        FeatureGroup {
            properties: Arc::new(properties),
            polygons: vec![],
            lines: vec![],
            points,
        }
    }

    #[test]
    fn a_tile_position_round_trips_through_longitude_and_latitude() {
        for (x, y) in [(0.0, 0.0), (1234.0, 3001.0), (-300.0, 4500.0)] {
            let (lng, lat) = lng_lat(x, y);
            assert_eq!(
                project(TILE, EXTENT, lng, lat),
                Some((x as i32, y as i32)),
                "({x}, {y})"
            );
        }
    }

    #[test]
    fn the_target_is_read_from_numbers_or_numeric_strings() {
        let (lng, lat) = lng_lat(100.0, 200.0);
        let num = group(
            &[
                ("lng", maplibre_expr::Value::Number(lng)),
                ("lat", maplibre_expr::Value::Number(lat)),
            ],
            vec![],
        );
        assert_eq!(target_of(&num, "lng", "lat"), Some((lng, lat)));
        let text = group(
            &[
                ("lng", maplibre_expr::Value::String(format!(" {lng} "))),
                ("lat", maplibre_expr::Value::String(lat.to_string())),
            ],
            vec![],
        );
        assert_eq!(target_of(&text, "lng", "lat"), Some((lng, lat)));
    }

    #[test]
    fn missing_or_invalid_fields_give_no_target() {
        let n = maplibre_expr::Value::Number;
        let s = |v: &str| maplibre_expr::Value::String(v.into());
        for props in [
            vec![("lng", n(139.7))],
            vec![("lat", n(35.6))],
            vec![("lng", n(139.7)), ("lat", s("north"))],
            vec![("lng", maplibre_expr::Value::Bool(true)), ("lat", n(35.6))],
            vec![("lng", n(200.0)), ("lat", n(35.6))],
            vec![("lng", n(139.7)), ("lat", n(-91.0))],
            vec![("lng", n(f64::NAN)), ("lat", n(35.6))],
        ] {
            assert_eq!(
                target_of(&group(&props, vec![]), "lng", "lat"),
                None,
                "{props:?}"
            );
        }
    }

    #[test]
    fn a_target_absurdly_far_off_is_dropped() {
        assert_eq!(
            project(TileId { z: 22, x: 0, y: 0 }, EXTENT, 179.0, 0.0),
            None
        );
    }
}
