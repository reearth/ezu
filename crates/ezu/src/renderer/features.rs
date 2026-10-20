//! Reading a node's features back out of a render.
//!
//! A map is often wanted alongside a machine-readable account of what is on
//! it: every label with what placement did to it, every feature a filter
//! kept. Those are the values of the graph's `features`-producing nodes, and
//! the evaluator computes them anyway on its way to the pixels, so
//! [`Renderer::render_tile_with_features`](super::Renderer::render_tile_with_features)
//! keeps the ones a host names instead of rendering a second time.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use ezu_graph::{NodeIx, NodeObserver, OpaqueValue, PortValue};

/// The value a `Features` port carries: one [`FeatureGroup`] per feature,
/// in tile units. Re-exported so a shell reading
/// [`NodeFeatures::features`] needs this crate and nothing under it.
pub use ezu_paint::nodes::{FeatureGroup, FilteredFeatures};

/// What one features node held when a render finished.
///
/// Geometry is in the tile's own units, as the node saw it: `x` right and
/// `y` down from the tile's top-left corner, `0..extent` across the tile.
/// Labels and other features that reach past the tile edge carry
/// coordinates outside that range.
#[derive(Clone)]
pub struct NodeFeatures {
    pub(super) node: String,
    pub(super) features: Option<Arc<FilteredFeatures>>,
}

impl NodeFeatures {
    /// The node's id, as the caller named it.
    pub fn node(&self) -> &str {
        &self.node
    }

    /// The node's value, or `None` when the render never reached the node —
    /// a branch a `switch` did not take, say.
    pub fn features(&self) -> Option<&FilteredFeatures> {
        self.features.as_deref()
    }

    /// The value as a GeoJSON FeatureCollection in tile units, with the
    /// node's id as a `node` member and the tile's size in those units as
    /// `extent`. A node the render never reached has no `extent` and no
    /// features.
    ///
    /// Each group is one Feature. A group holding one geometry kind is a
    /// single or multi geometry of that kind, one mixing kinds a
    /// GeometryCollection, and one with no geometry has a `null` geometry.
    /// Rings are closed and wound as RFC 7946 asks, exterior anticlockwise
    /// as drawn on the map. Coordinates are whole numbers; property values
    /// JSON has no form for — colours, formatted text — are written as
    /// their text.
    pub fn to_geojson(&self) -> String {
        let mut out = String::from(r#"{"type":"FeatureCollection","node":"#);
        out.push_str(&serde_json::Value::from(self.node.as_str()).to_string());
        let Some(features) = &self.features else {
            out.push_str(r#","features":[]}"#);
            return out;
        };
        let _ = write!(out, r#","extent":{},"features":["#, features.extent);
        for (i, group) in features.groups.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(r#"{"type":"Feature","geometry":"#);
            out.push_str(&geometry(group, &tile_position));
            out.push_str(r#","properties":"#);
            let props: serde_json::Map<String, serde_json::Value> = group
                .properties
                .iter()
                .map(|(k, v)| (k.clone(), property_json(v)))
                .collect();
            out.push_str(&serde_json::Value::Object(props).to_string());
            out.push('}');
        }
        out.push_str("]}");
        out
    }
}

impl std::fmt::Debug for NodeFeatures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeFeatures")
            .field("node", &self.node)
            .field("groups", &self.features.as_ref().map(|f| f.groups.len()))
            .finish()
    }
}

/// Keeps the value of each watched node from one render.
pub(super) struct Capture {
    nodes: Vec<NodeIx>,
    values: Mutex<Vec<Option<Arc<FilteredFeatures>>>>,
}

impl Capture {
    pub(super) fn new(nodes: Vec<NodeIx>) -> Self {
        let values = Mutex::new(vec![None; nodes.len()]);
        Capture { nodes, values }
    }

    /// The captured values, one per watched node in the order given to
    /// [`Capture::new`].
    pub(super) fn take(self) -> Vec<Option<Arc<FilteredFeatures>>> {
        self.values.into_inner().unwrap_or_else(|p| p.into_inner())
    }
}

impl NodeObserver for Capture {
    fn on_node(&self, ix: NodeIx, value: &PortValue, _cache_hit: bool, _elapsed_us: u128) {
        let PortValue::Features(any) = value else {
            return;
        };
        let Some(features) = downcast(any) else {
            return;
        };
        let mut values = self.values.lock().unwrap_or_else(|p| p.into_inner());
        // One node may be named twice; each slot gets the value.
        for (slot, &n) in self.nodes.iter().enumerate() {
            if n == ix {
                values[slot] = Some(features.clone());
            }
        }
    }
}

fn downcast(any: &OpaqueValue) -> Option<Arc<FilteredFeatures>> {
    Arc::clone(any).downcast::<FilteredFeatures>().ok()
}

/// A property value as JSON. A whole number is written without a fraction,
/// so a feature id reads `12`, not `12.0`; kinds JSON has no form for
/// (colours, formatted text) are written as their text.
pub(super) fn property_json(v: &maplibre_expr::Value) -> serde_json::Value {
    use maplibre_expr::Value;
    use serde_json::Value as J;
    match v {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Number(n) => number(*n),
        Value::String(s) => J::String(s.clone()),
        Value::Array(a) => J::Array(a.iter().map(property_json).collect()),
        Value::Object(o) => J::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), property_json(v)))
                .collect(),
        ),
        Value::NumberArray(a) => J::Array(a.iter().map(|n| number(*n)).collect()),
        Value::Padding(a) => J::Array(a.iter().map(|n| number(*n)).collect()),
        other => J::String(other.to_string()),
    }
}

fn number(n: f64) -> serde_json::Value {
    // Exactly representable integers only: past 2^53 an f64 no longer
    // holds every integer, and the shortest decimal form is the honest one.
    const EXACT: f64 = 9_007_199_254_740_992.0;
    if n.fract() == 0.0 && n.abs() < EXACT {
        serde_json::Value::from(n as i64)
    } else {
        serde_json::Number::from_f64(n).map_or(serde_json::Value::Null, Into::into)
    }
}

/// `g`'s geometry as GeoJSON text, each position written by `position`; a
/// group mixing kinds becomes a GeometryCollection, one with none `null`.
///
/// Tile units and page coordinates differ only in how a position is
/// written, so both go through here: the structure, the closing and the
/// winding of rings are the same.
pub(super) fn geometry(g: &FeatureGroup, position: &impl Fn(&mut String, (i32, i32))) -> String {
    let w = Writer { position };
    let mut parts = Vec::new();
    let mut s = String::new();
    match g.polygons.as_slice() {
        [] => {}
        [p] => {
            s.push_str(r#"{"type":"Polygon","coordinates":"#);
            w.polygon(&mut s, p);
            s.push('}');
            parts.push(std::mem::take(&mut s));
        }
        ps => {
            s.push_str(r#"{"type":"MultiPolygon","coordinates":["#);
            for (i, p) in ps.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                w.polygon(&mut s, p);
            }
            s.push_str("]}");
            parts.push(std::mem::take(&mut s));
        }
    }
    match g.lines.as_slice() {
        [] => {}
        [l] => {
            s.push_str(r#"{"type":"LineString","coordinates":"#);
            w.positions(&mut s, l.iter().copied());
            s.push('}');
            parts.push(std::mem::take(&mut s));
        }
        ls => {
            s.push_str(r#"{"type":"MultiLineString","coordinates":["#);
            for (i, l) in ls.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                w.positions(&mut s, l.iter().copied());
            }
            s.push_str("]}");
            parts.push(std::mem::take(&mut s));
        }
    }
    match g.points.as_slice() {
        [] => {}
        [p] => {
            s.push_str(r#"{"type":"Point","coordinates":"#);
            (w.position)(&mut s, *p);
            s.push('}');
            parts.push(std::mem::take(&mut s));
        }
        ps => {
            s.push_str(r#"{"type":"MultiPoint","coordinates":"#);
            w.positions(&mut s, ps.iter().copied());
            s.push('}');
            parts.push(std::mem::take(&mut s));
        }
    }
    match parts.len() {
        0 => "null".to_string(),
        1 => parts.pop().unwrap_or_default(),
        _ => format!(
            r#"{{"type":"GeometryCollection","geometries":[{}]}}"#,
            parts.join(",")
        ),
    }
}

/// A position in tile units, as written.
pub(super) fn tile_position(out: &mut String, (x, y): (i32, i32)) {
    let _ = write!(out, "[{x},{y}]");
}

struct Writer<'a, P> {
    position: &'a P,
}

impl<P: Fn(&mut String, (i32, i32))> Writer<'_, P> {
    fn positions(&self, out: &mut String, cs: impl IntoIterator<Item = (i32, i32)>) {
        out.push('[');
        for (i, c) in cs.into_iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            (self.position)(out, c);
        }
        out.push(']');
    }

    /// A ring closed and wound as RFC 7946 asks — exterior anticlockwise,
    /// holes clockwise, as seen on a north-up map — whatever order the
    /// node emitted it in. The winding is taken in tile units, which every
    /// written form is an orientation-keeping map of.
    fn ring(&self, out: &mut String, ring: &[(i32, i32)], exterior: bool) {
        let mut pts = ring.to_vec();
        if pts.len() > 1 && pts.first() != pts.last() {
            pts.push(pts[0]);
        }
        // Shoelace in y-down units: positive is clockwise on screen.
        let area: i128 = pts
            .windows(2)
            .map(|w| w[0].0 as i128 * w[1].1 as i128 - w[1].0 as i128 * w[0].1 as i128)
            .sum();
        if (area > 0) == exterior {
            pts.reverse();
        }
        self.positions(out, pts);
    }

    fn polygon(&self, out: &mut String, p: &ezu_features::Polygon) {
        out.push('[');
        self.ring(out, &p.exterior, true);
        for hole in &p.holes {
            out.push(',');
            self.ring(out, hole, false);
        }
        out.push(']');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn group(
        props: &[(&str, maplibre_expr::Value)],
        polygons: Vec<ezu_features::Polygon>,
        lines: Vec<Vec<(i32, i32)>>,
        points: Vec<(i32, i32)>,
    ) -> FeatureGroup {
        FeatureGroup {
            properties: Arc::new(
                props
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect::<BTreeMap<_, _>>(),
            ),
            polygons,
            lines,
            points,
        }
    }

    #[test]
    fn a_node_reads_as_a_feature_collection_in_tile_units() {
        use maplibre_expr::Value;
        let square = ezu_features::Polygon {
            // Clockwise on screen, so it is reversed to anticlockwise.
            exterior: vec![(0, 0), (10, 0), (10, 10), (0, 10)],
            holes: vec![],
        };
        let features = FilteredFeatures {
            extent: 4096,
            groups: vec![
                group(
                    &[
                        ("id", Value::Number(12.0)),
                        ("name", Value::String("A".into())),
                    ],
                    vec![],
                    vec![],
                    vec![(5, -3)],
                ),
                group(&[], vec![square], vec![vec![(1, 2), (3, 4)]], vec![]),
                group(&[], vec![], vec![], vec![]),
            ],
            overzoom: 0,
        };
        let nf = NodeFeatures {
            node: "boxes".into(),
            features: Some(Arc::new(features)),
        };
        assert_eq!(
            nf.to_geojson(),
            concat!(
                r#"{"type":"FeatureCollection","node":"boxes","extent":4096,"features":["#,
                r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[5,-3]},"properties":{"id":12,"name":"A"}},"#,
                r#"{"type":"Feature","geometry":{"type":"GeometryCollection","geometries":["#,
                r#"{"type":"Polygon","coordinates":[[[0,0],[0,10],[10,10],[10,0],[0,0]]]},"#,
                r#"{"type":"LineString","coordinates":[[1,2],[3,4]]}]},"properties":{}},"#,
                r#"{"type":"Feature","geometry":null,"properties":{}}]}"#,
            )
        );
        let unreached = NodeFeatures {
            node: "x".into(),
            features: None,
        };
        assert_eq!(
            unreached.to_geojson(),
            r#"{"type":"FeatureCollection","node":"x","features":[]}"#
        );
    }
}
