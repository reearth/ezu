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
//! A polygon gets a line that stops at its edge instead of running into
//! it: from the other end towards the polygon's centroid, ending where
//! that way first meets the exterior ring. This is the leader for a label
//! that placement moved off its first position, drawn to the label's box
//! from `label-boxes` so that it does not run under the text. A group with
//! several polygons gets one line per polygon. Holes are ignored, so an
//! end inside a hole counts as inside, and an end inside or on the
//! polygon yields no line, as there is no edge to stop at.
//!
//! `gap-px` pulls the feature end of every line back by that many pixels,
//! leaving some air between the leader and the box or point it reaches.
//! The other end stays put; a line no longer than the gap is dropped.
//!
//! A connector reaches into a tile from points outside it, which a tile's
//! thin buffer may not carry; `stitch: "pieces"` on the upstream
//! `features` node brings in the neighbouring tiles' points too.

use ezu_core::coord::{lat_to_world_y, lon_to_world_x};
use ezu_features::ops::centroid::polygon_centroid;
use ezu_features::Polygon;
use ezu_graph::{
    schema_frag, take_input_ref, BuiltNode, Connection, CoordSpace, EvalCtx, EvalError, FactoryCtx,
    FactoryError, In, InReader, InfluenceCtx, Node, NodeFactory, PortKind, PortSpec, PortValue,
    TileId,
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
    gap_px: In<f64>,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for SegmentToNode {
    fn op_name(&self) -> &'static str {
        "segment-to"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
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
        let scale = f64::from(feats.extent) / f64::from(ctx.canvas.tile_w.max(1));
        // `max` also turns a NaN from a port into no gap.
        let gap = ctx.px(self.gap_px.get(ctx, inputs)?.max(0.0)) * scale;
        let mut out_groups = Vec::new();
        for g in &feats.groups {
            let Some(target) = target_of(g, &self.lng_field, &self.lat_field)
                .and_then(|(lng, lat)| project(ctx.tile, feats.extent, lng, lat))
            else {
                continue;
            };
            let to_points = g.points.iter().filter_map(|&(x, y)| {
                let end = pull_back(target, (f64::from(x), f64::from(y)), gap)?;
                Some(vec![target, end])
            });
            let to_polygons = g.polygons.iter().filter_map(|p| {
                let edge = edge_towards_centroid(target, p)?;
                let end = pull_back(target, edge, gap)?;
                Some(vec![target, end])
            });
            let lines: Vec<Vec<(i32, i32)>> = to_points.chain(to_polygons).collect();
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
        self.gap_px.param_hash(h);
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

/// The end of a line from `from` to `to` once `gap` units are taken off
/// the `to` end, rounded to whole units. `None` when nothing is left, or
/// when the end rounds onto `from`. With no gap the end is `to` itself.
fn pull_back(from: (i32, i32), to: (f64, f64), gap: f64) -> Option<(i32, i32)> {
    let (fx, fy) = (f64::from(from.0), f64::from(from.1));
    let (dx, dy) = (to.0 - fx, to.1 - fy);
    let len = libm::hypot(dx, dy);
    if len <= gap {
        return None;
    }
    let k = (len - gap) / len;
    let end = ((fx + dx * k).round() as i32, (fy + dy * k).round() as i32);
    (end != from).then_some(end)
}

/// Where the way from `from` to the centroid of `polygon`'s exterior ring
/// first meets that ring. `None` when `from` lies inside the ring or on it,
/// or when the way never meets it (a ring too thin to have an inside, or
/// one whose centroid falls outside it and is reached around it rather
/// than across).
fn edge_towards_centroid(from: (i32, i32), polygon: &Polygon) -> Option<(f64, f64)> {
    let ring = &polygon.exterior;
    if ring.len() < 3 || inside_or_on(from, ring) {
        return None;
    }
    let c = polygon_centroid(polygon)?;
    let (ex, ey) = (f64::from(from.0), f64::from(from.1));
    let (dx, dy) = (f64::from(c.0) - ex, f64::from(c.1) - ey);
    let cross = |ax: f64, ay: f64, bx: f64, by: f64| ax * by - ay * bx;
    // The smallest fraction of the way along which an edge is crossed.
    let mut first: Option<f64> = None;
    for (i, &(ax, ay)) in ring.iter().enumerate() {
        let (bx, by) = ring[(i + 1) % ring.len()];
        let (ax, ay) = (f64::from(ax), f64::from(ay));
        let (fx, fy) = (f64::from(bx) - ax, f64::from(by) - ay);
        let denom = cross(dx, dy, fx, fy);
        if denom == 0.0 {
            // Parallel to the way, or a repeated vertex: the edges either
            // side of it are crossed instead.
            continue;
        }
        let (wx, wy) = (ax - ex, ay - ey);
        let t = cross(wx, wy, fx, fy) / denom;
        let u = cross(wx, wy, dx, dy) / denom;
        if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
            first = Some(first.map_or(t, |f| f.min(t)));
        }
    }
    first.map(|t| (ex + dx * t, ey + dy * t))
}

/// Whether `p` lies inside `ring` (even-odd) or on one of its edges.
fn inside_or_on(p: (i32, i32), ring: &[(i32, i32)]) -> bool {
    let (px, py) = (i64::from(p.0), i64::from(p.1));
    let mut inside = false;
    for (i, &(ax, ay)) in ring.iter().enumerate() {
        let (bx, by) = ring[(i + 1) % ring.len()];
        let (ax, ay, bx, by) = (i64::from(ax), i64::from(ay), i64::from(bx), i64::from(by));
        let on_line = (bx - ax) * (py - ay) - (by - ay) * (px - ax) == 0;
        if on_line && px >= ax.min(bx) && px <= ax.max(bx) && py >= ay.min(by) && py <= ay.max(by) {
            return true;
        }
        if (ay > py) != (by > py) {
            // x where the edge crosses the horizontal through `p`, compared
            // without dividing: px < ax + (py - ay) * (bx - ax) / (by - ay).
            let lhs = (px - ax) * (by - ay);
            let rhs = (py - ay) * (bx - ax);
            if (by > ay && lhs < rhs) || (by < ay && lhs > rhs) {
                inside = !inside;
            }
        }
    }
    inside
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
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let features = take_input_ref(fields, "features")?;
        let lng_field = read_optional_string(fields, "lng-field")?
            .ok_or_else(|| FactoryError::MissingField("lng-field".into()))?;
        let lat_field = read_optional_string(fields, "lat-field")?
            .ok_or_else(|| FactoryError::MissingField("lat-field".into()))?;
        let mut r = InReader::new(fields, ctx, 1);
        let gap_px = r.number_or("gap-px", 0.0)?;
        let parts = r.finish();
        // A literal (or a `$param` whose declared `max`) below zero can
        // only be a mistake; a `@node` port is clamped at eval time.
        if gap_px.static_bound().is_some_and(|b| b < 0.0) {
            return Err(FactoryError::BadField {
                field: "gap-px".into(),
                msg: "gap-px must be >= 0".into(),
            });
        }

        let mut ports = vec![PortSpec {
            name: "features",
            accepts: &[PortKind::Features],
            optional: false,
        }];
        ports.extend(parts.ports);
        let mut connections = vec![Connection {
            port: "features".into(),
            src: features,
        }];
        connections.extend(parts.connections);
        Ok(BuiltNode {
            node: Box::new(SegmentToNode {
                lng_field,
                lat_field,
                gap_px,
                ports,
                param_refs: parts.param_refs,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Turn each point into a two-vertex polyline from a position its feature's properties give in longitude and latitude to the point itself: a leader from a label anchor to what it labels, a connector from a building's representative point to its entrance. A polygon gets a line from that position towards its centroid that stops where it first meets the polygon's outer edge, so a leader to a label box from `label-boxes` ends at the box instead of running under the text; one line per polygon, and none when the position lies inside or on the polygon (holes count as inside). Each line keeps its feature's properties; dash it with `dash` and paint it with `stroke`. Lines are ignored, as are features whose fields are missing, not numbers (or numeric strings), or out of range, and points that coincide with their other end. A connector can cross a tile from a point outside it, beyond the tile's thin buffer; set `stitch: \"pieces\"` on the upstream `features` node to bring in the neighbouring tiles' points too.",
            "properties": {
                "features": schema_frag::node_ref(),
                "lng-field": { "type": "string",
                    "description": "Property holding the other end's longitude, in WGS84 degrees." },
                "lat-field": { "type": "string",
                    "description": "Property holding the other end's latitude, in WGS84 degrees." },
                "gap-px": schema_frag::in_number(serde_json::json!({ "type": "number", "minimum": 0.0, "default": 0.0,
                    "description": "Pixels taken off the feature end of each line, at the point or the polygon's edge, to leave air between the line and what it reaches. The other end stays put; a line no longer than the gap is dropped." })),
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

    fn square(x0: i32, y0: i32, x1: i32, y1: i32) -> Polygon {
        Polygon {
            exterior: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)],
            holes: vec![],
        }
    }

    #[test]
    fn the_way_to_a_polygon_ends_at_its_nearer_edge() {
        let sq = square(100, 100, 200, 200);
        assert_eq!(edge_towards_centroid((0, 150), &sq), Some((100.0, 150.0)));
        assert_eq!(edge_towards_centroid((150, 400), &sq), Some((150.0, 200.0)));
        // Diagonally in through the corner.
        assert_eq!(edge_towards_centroid((50, 50), &sq), Some((100.0, 100.0)));
        // Wound the other way, the same edge.
        let mut ccw = sq.clone();
        ccw.exterior.reverse();
        assert_eq!(edge_towards_centroid((0, 150), &ccw), Some((100.0, 150.0)));
    }

    #[test]
    fn an_end_inside_or_on_a_polygon_has_no_edge_to_stop_at() {
        let sq = square(100, 100, 200, 200);
        for e in [(150, 150), (101, 199), (100, 150), (200, 200), (150, 100)] {
            assert_eq!(edge_towards_centroid(e, &sq), None, "{e:?}");
        }
        assert!(!inside_or_on((99, 150), &sq.exterior));
        assert!(!inside_or_on((250, 100), &sq.exterior));
    }

    #[test]
    fn a_gap_pulls_the_far_end_back_along_the_line() {
        assert_eq!(pull_back((0, 0), (100.0, 0.0), 0.0), Some((100, 0)));
        assert_eq!(pull_back((0, 0), (100.0, 0.0), 30.0), Some((70, 0)));
        assert_eq!(pull_back((0, 0), (30.0, 40.0), 25.0), Some((15, 20)));
        assert_eq!(pull_back((0, 0), (100.0, 0.0), 100.0), None);
        assert_eq!(pull_back((5, 5), (5.0, 5.0), 0.0), None);
    }

    #[test]
    fn a_target_absurdly_far_off_is_dropped() {
        assert_eq!(
            project(TileId { z: 22, x: 0, y: 0 }, EXTENT, 179.0, 0.0),
            None
        );
    }
}
