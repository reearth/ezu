//! `graticule` — `() -> Features`. The parallels of latitude and
//! meridians of longitude crossing the current tile, as polylines.
//!
//! Web Mercator is a cylindrical projection, so both families are
//! straight and axis-aligned: a parallel is a horizontal line, a
//! meridian a vertical one. What the node really does is decide *which*
//! lines to draw and where they land.
//!
//! Each line comes back as its own feature group carrying `axis`
//! (`"parallel"` or `"meridian"`), `degrees`, and a formatted `label`,
//! so the lines can be drawn and labelled by the ops that already do
//! that:
//!
//! ```json
//! "grid":   { "op": "graticule" },
//! "lines":  { "op": "stroke", "features": "@grid", "width-px": 0.5, "color": "#00000033" },
//! "labels": { "op": "text", "features": "@grid", "text-expr": ["get", "label"], "size": 10 }
//! ```
//!
//! With no `interval-deg` the spacing is chosen from the zoom so that a
//! handful of lines cross the tile, stepping down the conventional
//! 30°/10°/5° ladder rather than an arbitrary fraction.

use ezu_core::coord::{lat_to_world_y, lon_to_world_x, world_y_to_lat, MERCATOR_MAX_LAT};
use ezu_graph::{
    schema_frag, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, In, InReader,
    Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use maplibre_expr::Value as ExprValue;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{features_value, FeatureGroup};

const DEFAULT_EXTENT: u32 = 4096;

/// Intervals a graticule is conventionally drawn at, in degrees:
/// thirties and tens, then the 5/2/1 decimal ladder below, down to about
/// a tenth of a metre so the deepest zooms still have a rung to stand on.
const LADDER: [f64; 23] = [
    30.0, 10.0, 5.0, 2.0, 1.0, 0.5, 0.2, 0.1, 0.05, 0.02, 0.01, 0.005, 0.002, 0.001, 5e-4, 2e-4,
    1e-4, 5e-5, 2e-5, 1e-5, 5e-6, 2e-6, 1e-6,
];

/// Which families of lines to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axes {
    Both,
    Parallels,
    Meridians,
}

/// The coarsest ladder interval that still puts about four lines across
/// a tile at zoom `z`, where a tile spans `360 / 2^z` degrees of
/// longitude.
fn auto_interval(z: u8) -> f64 {
    let target = 360.0 / (1u64 << z) as f64 / 4.0;
    *LADDER
        .iter()
        .find(|&&step| step <= target)
        .unwrap_or(LADDER.last().unwrap())
}

/// Format a signed degree value for display: magnitude, degree sign, and
/// a hemisphere letter, with zero left unsigned. Decimals are trimmed,
/// so a 5° graticule reads `40°N` and a 0.5° one `40.5°N`. The precision
/// covers the whole interval ladder, down to its millionths.
fn label_for(deg: f64, axis: Axes) -> String {
    let mut mag = format!("{:.6}", deg.abs());
    while mag.contains('.') && (mag.ends_with('0') || mag.ends_with('.')) {
        mag.pop();
    }
    // Within half a ladder step of zero this is the equator or the prime
    // meridian, which carry no hemisphere.
    if deg.abs() < 1e-9 {
        return format!("{mag}°");
    }
    let hemisphere = match (axis, deg > 0.0) {
        (Axes::Meridians, true) => "E",
        (Axes::Meridians, false) => "W",
        (_, true) => "N",
        (_, false) => "S",
    };
    format!("{mag}°{hemisphere}")
}

/// One graticule line as a feature group: the polyline plus the
/// properties that let a downstream op label it.
fn line_group(deg: f64, axis: Axes, line: Vec<(i32, i32)>) -> FeatureGroup {
    let mut properties = BTreeMap::new();
    properties.insert(
        "axis".to_string(),
        ExprValue::String(
            match axis {
                Axes::Meridians => "meridian",
                _ => "parallel",
            }
            .to_string(),
        ),
    );
    properties.insert("degrees".to_string(), ExprValue::Number(deg));
    properties.insert("label".to_string(), ExprValue::String(label_for(deg, axis)));
    FeatureGroup {
        properties: Arc::new(properties),
        polygons: vec![],
        lines: vec![line],
        points: vec![],
    }
}

struct GraticuleNode {
    extent: u32,
    interval_deg: Option<In<f64>>,
    axes: Axes,
    ports: Vec<PortSpec>,
    param_refs: Vec<String>,
}

impl Node for GraticuleNode {
    fn op_name(&self) -> &'static str {
        "graticule"
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
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let e = self.extent as f64;
        let interval = match &self.interval_deg {
            Some(field) => field.get(ctx, inputs)?,
            None => auto_interval(ctx.tile.z),
        };
        // A non-positive interval has no lines in it; emit nothing
        // rather than loop forever.
        if !interval.is_finite() || interval <= 0.0 {
            return Ok(features_value(self.extent, vec![]));
        }

        let axis_tiles = (1u64 << ctx.tile.z) as f64;
        // The tile's own footprint in world units, and the conversion
        // back into its local frame.
        let world_x0 = ctx.tile.x as f64 / axis_tiles;
        let world_y0 = ctx.tile.y as f64 / axis_tiles;
        let span = 1.0 / axis_tiles;
        let to_local = |world: f64, origin: f64| (world - origin) / span * e;

        let mut groups = Vec::new();

        if self.axes != Axes::Meridians {
            // North edge is the smaller world y, so it is the *higher*
            // latitude: walk south from there.
            let lat_north = world_y_to_lat(world_y0).min(MERCATOR_MAX_LAT);
            let lat_south = world_y_to_lat(world_y0 + span).max(-MERCATOR_MAX_LAT);
            let k_lo = (lat_south / interval).ceil() as i64;
            let k_hi = (lat_north / interval).floor() as i64;
            for k in k_lo..=k_hi {
                let deg = k as f64 * interval;
                let y = to_local(lat_to_world_y(deg), world_y0).round() as i32;
                groups.push(line_group(
                    deg,
                    Axes::Parallels,
                    vec![(0, y), (self.extent as i32, y)],
                ));
            }
        }

        if self.axes != Axes::Parallels {
            let lon_west = (world_x0 * 360.0 - 180.0).max(-180.0);
            let lon_east = ((world_x0 + span) * 360.0 - 180.0).min(180.0);
            let k_lo = (lon_west / interval).ceil() as i64;
            let k_hi = (lon_east / interval).floor() as i64;
            for k in k_lo..=k_hi {
                let deg = k as f64 * interval;
                let x = to_local(lon_to_world_x(deg), world_x0).round() as i32;
                groups.push(line_group(
                    deg,
                    Axes::Meridians,
                    vec![(x, 0), (x, self.extent as i32)],
                ));
            }
        }

        Ok(features_value(self.extent, groups))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"graticule");
        h.update(&self.extent.to_le_bytes());
        if let Some(field) = &self.interval_deg {
            field.param_hash(h);
        }
        h.update(match self.axes {
            Axes::Both => &[0u8],
            Axes::Parallels => &[1u8],
            Axes::Meridians => &[2u8],
        });
    }
    fn param_refs(&self) -> Vec<String> {
        self.param_refs.clone()
    }
}

pub(super) struct GraticuleFactory;
impl NodeFactory for GraticuleFactory {
    fn op_name(&self) -> &'static str {
        "graticule"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let extent = fields
            .get("extent")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .unwrap_or(DEFAULT_EXTENT);
        let axes = match fields.get("axes").and_then(Value::as_str) {
            None | Some("both") => Axes::Both,
            Some("parallels") => Axes::Parallels,
            Some("meridians") => Axes::Meridians,
            Some(other) => {
                return Err(FactoryError::BadField {
                    field: "axes".into(),
                    msg: format!("unknown axes '{other}', expected both/parallels/meridians"),
                });
            }
        };

        let mut r = InReader::new(fields, ctx, 0);
        // Absent rather than defaulted: without an interval the node
        // picks one from the zoom, which no constant can express.
        let interval_deg = match fields.contains_key("interval-deg") {
            true => Some(r.number("interval-deg")?),
            false => None,
        };
        let parts = r.finish();

        if let Some(field) = &interval_deg {
            if let Some(b) = field.static_bound() {
                if b <= 0.0 {
                    return Err(FactoryError::BadField {
                        field: "interval-deg".into(),
                        msg: "interval-deg must be > 0".into(),
                    });
                }
            }
        }

        Ok(BuiltNode {
            node: Box::new(GraticuleNode {
                extent,
                interval_deg,
                axes,
                ports: parts.ports,
                param_refs: parts.param_refs,
            }),
            connections: parts.connections,
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Parallels and meridians crossing the tile, as labelled polylines.",
            "properties": {
                "extent": { "type": "integer", "minimum": 1, "default": DEFAULT_EXTENT,
                            "description": "Coordinate extent of the emitted geometry, matching the features it is drawn alongside." },
                "interval-deg": schema_frag::in_number(serde_json::json!({ "type": "number", "exclusiveMinimum": 0.0,
                                   "description": "Spacing in degrees. Omit to pick one from the zoom along the 30/10/5/2/1 ladder." })),
                "axes": { "type": "string", "enum": ["both", "parallels", "meridians"], "default": "both",
                          "description": "Which families to emit: parallels of latitude, meridians of longitude, or both." },
            },
        })
    }
}

ezu_graph::submit_node!(GraticuleFactory);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_interval_walks_down_the_ladder() {
        // A tile spans 360° at z=0 and halves from there, so the
        // interval steps down as the ladder allows.
        assert_eq!(auto_interval(0), 30.0);
        assert_eq!(auto_interval(1), 30.0);
        assert_eq!(auto_interval(2), 10.0);
        assert_eq!(auto_interval(4), 5.0);
        assert_eq!(auto_interval(6), 1.0);
        // Every zoom picks something positive, including past the end of
        // the ladder.
        for z in 0..=24 {
            assert!(auto_interval(z) > 0.0, "z = {z}");
        }
    }

    #[test]
    fn auto_interval_keeps_a_handful_of_lines_per_tile() {
        for z in 0..=18 {
            let span = 360.0 / (1u64 << z) as f64;
            let lines = span / auto_interval(z);
            assert!(
                (4.0..=12.0).contains(&lines),
                "z = {z} draws {lines} meridians per tile"
            );
        }
    }

    #[test]
    fn labels_name_the_hemisphere() {
        assert_eq!(label_for(60.0, Axes::Parallels), "60°N");
        assert_eq!(label_for(-33.5, Axes::Parallels), "33.5°S");
        assert_eq!(label_for(139.75, Axes::Meridians), "139.75°E");
        assert_eq!(label_for(-74.0, Axes::Meridians), "74°W");
        // The equator and the prime meridian have no hemisphere.
        assert_eq!(label_for(0.0, Axes::Parallels), "0°");
        assert_eq!(label_for(0.0, Axes::Meridians), "0°");
    }
}
