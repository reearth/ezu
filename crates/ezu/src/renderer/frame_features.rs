//! A features node's value over a whole print frame, as one GeoJSON file.
//!
//! A print pipeline wants a machine-readable list of what is on the page —
//! every label, say, with what placement did to it — next to the image. The
//! node is evaluated per tile, so the same label shows up in each tile that
//! sees it; an optional key collapses those copies to one. The file has to
//! be byte-identical across runs of the same input, so tiles are read in a
//! fixed order whatever order they finished in, and numbers are written at
//! a fixed precision.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use maplibre_expr::Value;

use super::features::{geometry, property_json};
use super::{Error, ErrorKind, FeatureGroup, FilteredFeatures, Frame, NodeFeatures, TileId};

/// Decimal places for longitude and latitude: about 1 cm on the ground,
/// finer than any print pixel.
const LONLAT_DECIMALS: usize = 7;
/// Decimal places for page pixels.
const PX_DECIMALS: usize = 2;

/// The coordinates page features are written in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FeatureCoords {
    /// WGS84 longitude and latitude, as RFC 7946 GeoJSON expects, to seven
    /// decimal places.
    #[default]
    LonLat,
    /// Pixels of the page: origin at its top-left corner, `y` down, to two
    /// decimal places.
    Px,
}

impl FeatureCoords {
    /// The name the JS and Go packages spell it with: `"lonlat"` or `"px"`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::LonLat => "lonlat",
            Self::Px => "px",
        }
    }

    /// The coordinates [`name`](Self::name) spells, or `None`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "lonlat" => Some(Self::LonLat),
            "px" => Some(Self::Px),
            _ => None,
        }
    }
}

/// Which features nodes to gather over a frame, and how to write them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameFeatures {
    /// One page of features per entry, in this order. A node may be named
    /// more than once, with different keys; it is still captured once per
    /// tile.
    pub nodes: Vec<FrameFeatureNode>,
    pub coords: FeatureCoords,
}

/// One features node to gather over a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameFeatureNode {
    /// The node's id in the style.
    pub node: String,
    /// Properties that identify a feature across tiles. With a key, the
    /// features whose key properties are all equal are written once — the
    /// first in reading order — and the page is sorted by the key. Without
    /// one, every tile's features are written, so a feature several tiles
    /// see appears once per tile, and a shape cut at tile edges appears as
    /// its pieces.
    pub key: Option<Vec<String>>,
}

impl FrameFeatureNode {
    /// `node`, every tile's features kept.
    pub fn new(node: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            key: None,
        }
    }

    /// `node`, its features told apart by the properties in `key`.
    pub fn keyed(
        node: impl Into<String>,
        key: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            node: node.into(),
            key: Some(key.into_iter().map(Into::into).collect()),
        }
    }
}

/// What one features node held over a whole frame.
///
/// Each feature is one the node held in some tile whose bounding box
/// reaches into the page, written whole rather than clipped to it, in the
/// [`FeatureCoords`] asked for.
#[derive(Clone, Debug)]
pub struct PageFeatures {
    node: String,
    features: Vec<PageFeature>,
}

#[derive(Clone, Debug)]
struct PageFeature {
    properties: Arc<BTreeMap<String, Value>>,
    /// Already written: the projection is the frame's, not this value's.
    geometry: String,
}

impl PageFeatures {
    /// The node's id, as it was asked for.
    pub fn node(&self) -> &str {
        &self.node
    }

    /// How many features the page holds.
    pub fn len(&self) -> usize {
        self.features.len()
    }

    /// Whether the page holds no features.
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    /// The page as a GeoJSON FeatureCollection, exactly the file
    /// `ezu frame --features` writes: one Feature per line, a newline at
    /// the end, and the same bytes for the same input every time.
    ///
    /// A feature holding one geometry kind is a single or multi geometry of
    /// that kind, one mixing kinds a GeometryCollection. Rings are closed
    /// and wound as RFC 7946 asks, exterior anticlockwise as drawn on the
    /// map. Property values JSON has no form for — colours, formatted
    /// text — are written as their text.
    pub fn to_geojson(&self) -> String {
        let mut out = String::from(r#"{"type":"FeatureCollection","features":["#);
        for (i, f) in self.features.iter().enumerate() {
            out.push_str(if i == 0 { "\n" } else { ",\n" });
            let props: serde_json::Map<String, serde_json::Value> = f
                .properties
                .iter()
                .map(|(k, v)| (k.clone(), property_json(v)))
                .collect();
            out.push_str(r#"{"type":"Feature","geometry":"#);
            out.push_str(&f.geometry);
            out.push_str(r#","properties":"#);
            out.push_str(&serde_json::Value::Object(props).to_string());
            out.push('}');
        }
        if !self.features.is_empty() {
            out.push('\n');
        }
        out.push_str("]}\n");
        out
    }
}

/// Gathers features nodes' values over the tiles of a frame and assembles
/// them into [`PageFeatures`]. Made by
/// [`Renderer::frame_features`](super::Renderer::frame_features).
///
/// [`Renderer::render_frame_with_features`](super::Renderer::render_frame_with_features)
/// does all of this itself. The collector is for a host that renders a
/// frame's tiles its own way — several at once over one `&Renderer`, say,
/// which [`render_tile_with`](super::Renderer::render_tile_with) allows:
/// render each tile with
/// [`render_tile_with_features`](super::Renderer::render_tile_with_features)
/// for [`nodes`](Self::nodes), [`add`](Self::add) what comes back in
/// whatever order the tiles finish, and [`finish`](Self::finish).
///
/// The JS and Go packages have it as `frameFeatures` (`FrameFeatures`),
/// in a different shape: there a tile renders through the collector's
/// `renderTile` (`RenderTile`) instead of being added after, since the
/// values live in the module's memory and are kept there rather than
/// copied out to the host and back.
#[derive(Clone)]
pub struct FrameFeatureCollector {
    frame: Frame,
    coords: FeatureCoords,
    /// Each requested entry, with where its node's value sits in a tile's
    /// captures.
    entries: Vec<(FrameFeatureNode, usize)>,
    /// The distinct nodes, each captured once per tile.
    nodes: Vec<String>,
    /// Each tile added so far, as `(y, x)` and one value per node.
    tiles: Vec<((u32, u32), Captured)>,
}

/// One tile's value of each node, `None` where the render never reached it.
type Captured = Vec<Option<Arc<FilteredFeatures>>>;

impl FrameFeatureCollector {
    pub(super) fn new(frame: &Frame, req: &FrameFeatures) -> Self {
        let mut nodes: Vec<String> = Vec::new();
        let entries = req
            .nodes
            .iter()
            .map(|entry| {
                let slot = match nodes.iter().position(|n| *n == entry.node) {
                    Some(slot) => slot,
                    None => {
                        nodes.push(entry.node.clone());
                        nodes.len() - 1
                    }
                };
                (entry.clone(), slot)
            })
            .collect();
        Self {
            frame: frame.clone(),
            coords: req.coords,
            entries,
            nodes,
            tiles: Vec::new(),
        }
    }

    /// The nodes to render each tile's features for: every node asked for,
    /// once each.
    pub fn nodes(&self) -> Vec<&str> {
        self.nodes.iter().map(String::as_str).collect()
    }

    /// Keep what `tile` — one of the frame's — rendered with. `values` is
    /// what [`render_tile_with_features`](super::Renderer::render_tile_with_features)
    /// handed back for [`nodes`](Self::nodes); a node missing from it is
    /// taken as one the render never reached.
    pub fn add(&mut self, tile: TileId, values: &[NodeFeatures]) {
        let captured = self
            .nodes
            .iter()
            .map(|n| {
                values
                    .iter()
                    .find(|v| v.node == *n)
                    .and_then(|v| v.features.clone())
            })
            .collect();
        self.tiles.push(((tile.y, tile.x), captured));
    }

    /// One page per entry asked for, in that order.
    ///
    /// Tiles are read rows top to bottom, then left to right, and each
    /// tile's features in their own order. A feature whose bounding box
    /// lies wholly outside the page is dropped; one that reaches into it is
    /// kept whole, not clipped. With a key, the first feature of each key
    /// in that reading order is kept and the page is sorted by the key:
    /// missing values first, then null, booleans, numbers (numerically),
    /// strings, and anything else by its JSON text.
    pub fn finish(mut self) -> Vec<PageFeatures> {
        self.tiles.sort_by_key(|(at, _)| *at);
        self.entries
            .iter()
            .map(|(entry, slot)| self.page(entry, *slot))
            .collect()
    }

    fn page(&self, entry: &FrameFeatureNode, slot: usize) -> PageFeatures {
        let frame = &self.frame;
        let mut items: Vec<(Vec<Option<&Value>>, PageFeature)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for ((y, x), values) in &self.tiles {
            let Some(feats) = values.get(slot).and_then(Option::as_deref) else {
                continue;
            };
            let project = Projection::new(frame, *x, *y, feats.extent, self.coords);
            for group in &feats.groups {
                let Some(bounds) = project.frame_bounds(group) else {
                    continue;
                };
                if !bounds.reaches_into(frame) {
                    continue;
                }
                let key: Vec<Option<&Value>> = match &entry.key {
                    Some(props) => props.iter().map(|p| group.properties.get(p)).collect(),
                    None => Vec::new(),
                };
                if entry.key.is_some() && !seen.insert(identity(&key)) {
                    continue;
                }
                let geometry = geometry(group, &|out: &mut String, c| project.position(out, c));
                items.push((
                    key,
                    PageFeature {
                        properties: Arc::clone(&group.properties),
                        geometry,
                    },
                ));
            }
        }
        if entry.key.is_some() {
            // Stable, so equal keys keep the reading order.
            items.sort_by(|a, b| compare_keys(&a.0, &b.0));
        }
        PageFeatures {
            node: entry.node.clone(),
            features: items.into_iter().map(|(_, f)| f).collect(),
        }
    }
}

impl std::fmt::Debug for FrameFeatureCollector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameFeatureCollector")
            .field("coords", &self.coords)
            .field("entries", &self.entries)
            .field("tiles", &self.tiles.len())
            .finish()
    }
}

/// Check `req` before anything renders: every key names at least one
/// property. The nodes themselves are checked against the graph by the
/// renderer.
pub(super) fn check_request(req: &FrameFeatures) -> Result<(), Error> {
    for entry in &req.nodes {
        if let Some(key) = &entry.key {
            if key.is_empty() || key.iter().any(String::is_empty) {
                return Err(Error::new(
                    ErrorKind::InvalidStyle,
                    format!(
                        "the key for node `{}` must name one or more properties",
                        entry.node
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// A key as text, distinct for every distinct key: an absent property
/// differs from a `null` one, and `1` equals `1.0`.
fn identity(key: &[Option<&Value>]) -> String {
    let mut s = String::new();
    for v in key {
        match v {
            None => s.push('-'),
            Some(v) => {
                s.push('=');
                s.push_str(&property_json(v).to_string());
            }
        }
        s.push('\u{1f}');
    }
    s
}

/// Missing values first, then null, booleans, numbers (numerically),
/// strings, and anything else by its JSON text.
fn compare_keys(a: &[Option<&Value>], b: &[Option<&Value>]) -> Ordering {
    fn rank(v: Option<&Value>) -> u8 {
        match v {
            None => 0,
            Some(Value::Null) => 1,
            Some(Value::Bool(_)) => 2,
            Some(Value::Number(_)) => 3,
            Some(Value::String(_)) => 4,
            Some(_) => 5,
        }
    }
    for (x, y) in a.iter().zip(b) {
        let ord = rank(*x).cmp(&rank(*y)).then_with(|| match (x, y) {
            (Some(Value::Bool(x)), Some(Value::Bool(y))) => x.cmp(y),
            (Some(Value::Number(x)), Some(Value::Number(y))) => x.total_cmp(y),
            (Some(Value::String(x)), Some(Value::String(y))) => x.cmp(y),
            (Some(x), Some(y)) => property_json(x)
                .to_string()
                .cmp(&property_json(y).to_string()),
            _ => Ordering::Equal,
        });
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// Bounding box in page pixels.
struct Bounds {
    min: (f64, f64),
    max: (f64, f64),
}

impl Bounds {
    fn reaches_into(&self, frame: &Frame) -> bool {
        self.max.0 >= 0.0
            && self.max.1 >= 0.0
            && self.min.0 <= frame.width as f64
            && self.min.1 <= frame.height as f64
    }
}

/// From one tile's geometry units to the written coordinates.
struct Projection {
    /// Tile origin in page pixels.
    origin: (f64, f64),
    /// Page pixels per geometry unit.
    per_unit: f64,
    /// Frame corner in world pixels, and the world's width in them.
    left: f64,
    top: f64,
    world: f64,
    coords: FeatureCoords,
}

impl Projection {
    fn new(frame: &Frame, x: u32, y: u32, extent: u32, coords: FeatureCoords) -> Self {
        let (ox, oy) = frame.tile_offset(x, y);
        let t = frame.tile_size as f64;
        Projection {
            origin: (ox as f64, oy as f64),
            per_unit: t / extent.max(1) as f64,
            left: frame.left as f64,
            top: frame.top as f64,
            world: t * (1u64 << frame.zoom) as f64,
            coords,
        }
    }

    /// `(u, v)` in geometry units → page pixels.
    fn px(&self, (u, v): (i32, i32)) -> (f64, f64) {
        (
            self.origin.0 + u as f64 * self.per_unit,
            self.origin.1 + v as f64 * self.per_unit,
        )
    }

    fn frame_bounds(&self, g: &FeatureGroup) -> Option<Bounds> {
        let all = g
            .polygons
            .iter()
            .flat_map(|p| p.exterior.iter())
            .chain(g.lines.iter().flatten())
            .chain(g.points.iter());
        let mut b: Option<Bounds> = None;
        for &c in all {
            let (x, y) = self.px(c);
            let b = b.get_or_insert(Bounds {
                min: (x, y),
                max: (x, y),
            });
            b.min = (b.min.0.min(x), b.min.1.min(y));
            b.max = (b.max.0.max(x), b.max.1.max(y));
        }
        b
    }

    /// One position as GeoJSON text.
    fn position(&self, out: &mut String, c: (i32, i32)) {
        let (x, y) = self.px(c);
        let (a, b, decimals) = match self.coords {
            FeatureCoords::Px => (x, y, PX_DECIMALS),
            FeatureCoords::LonLat => (
                ezu_core::coord::world_x_to_lon((x + self.left) / self.world),
                ezu_core::coord::world_y_to_lat((y + self.top) / self.world),
                LONLAT_DECIMALS,
            ),
        };
        out.push('[');
        out.push_str(&fixed(a, decimals));
        out.push(',');
        out.push_str(&fixed(b, decimals));
        out.push(']');
    }
}

/// `v` to `decimals` places, never as `-0.00`.
fn fixed(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    match s.strip_prefix('-') {
        Some(rest) if rest.bytes().all(|b| b == b'0' || b == b'.') => rest.to_string(),
        _ => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::FrameRequest;

    fn frame() -> Frame {
        Frame::compute(&FrameRequest {
            center_lon: 139.70,
            center_lat: 35.66,
            scale: 5000.0,
            dpi: 96.0,
            width: 300,
            height: 200,
            zoom: None,
            style_tile_size: 256,
        })
        .expect("a frame")
    }

    fn point(props: &[(&str, Value)], at: (i32, i32)) -> FeatureGroup {
        FeatureGroup {
            properties: Arc::new(
                props
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect(),
            ),
            polygons: vec![],
            lines: vec![],
            points: vec![at],
        }
    }

    fn values(node: &str, groups: Vec<FeatureGroup>) -> Vec<NodeFeatures> {
        vec![NodeFeatures {
            node: node.into(),
            features: Some(Arc::new(FilteredFeatures {
                extent: 4096,
                groups,
                overzoom: 0,
            })),
        }]
    }

    /// Copies of one feature from two tiles are one with a key, and the
    /// page is sorted by it; the order tiles are added in does not matter.
    #[test]
    fn a_key_keeps_the_first_copy_in_reading_order_and_sorts() {
        let f = frame();
        let (x0, y0) = (f.tiles_x.start, f.tiles_y.start);
        // The middle of the first tile on the page, in its own units.
        let (ox, oy) = f.tile_offset(x0 + 1, y0 + 1);
        let unit = 4096.0 / f.tile_size as f64;
        let mid = (
            ((50.0 - ox as f64) * unit) as i32,
            ((50.0 - oy as f64) * unit) as i32,
        );
        let id = |n: f64| [("id", Value::Number(n))];
        let req = FrameFeatures {
            nodes: vec![
                FrameFeatureNode::keyed("pts", ["id"]),
                FrameFeatureNode::new("pts"),
            ],
            coords: FeatureCoords::Px,
        };
        let mut c = FrameFeatureCollector::new(&f, &req);
        assert_eq!(c.nodes(), ["pts"]);
        // Added out of reading order: the right-hand tile first, its copy
        // of `2` a little right of the left tile's.
        c.add(
            TileId::new(f.zoom, x0 + 2, y0 + 1),
            &values("pts", vec![point(&id(2.0), (mid.0 - 4096 + 400, mid.1))]),
        );
        c.add(
            TileId::new(f.zoom, x0 + 1, y0 + 1),
            &values(
                "pts",
                vec![
                    point(&id(10.0), mid),
                    point(&id(2.0), mid),
                    // Far off the page: dropped.
                    point(&id(3.0), (mid.0 + 100_000, mid.1)),
                ],
            ),
        );
        let [keyed, raw] = c.finish().try_into().expect("two pages");
        assert_eq!((keyed.node(), keyed.len(), raw.len()), ("pts", 2, 3));
        let text = keyed.to_geojson();
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let ids: Vec<i64> = doc["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["properties"]["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, [2, 10], "sorted numerically, not as text");
        assert!(text.starts_with("{\"type\":\"FeatureCollection\",\"features\":[\n{"));
        assert!(text.ends_with("}\n]}\n"));
        // The kept copy of `2` is the left tile's, which is read first.
        let at = &doc["features"][0]["geometry"]["coordinates"];
        assert!((at[0].as_f64().unwrap() - 50.0).abs() < 0.5, "{at}");
    }

    #[test]
    fn an_empty_page_is_still_a_collection() {
        let c = FrameFeatureCollector::new(
            &frame(),
            &FrameFeatures {
                nodes: vec![FrameFeatureNode::new("x")],
                coords: FeatureCoords::LonLat,
            },
        );
        let [page] = c.finish().try_into().expect("one page");
        assert!(page.is_empty());
        assert_eq!(
            page.to_geojson(),
            "{\"type\":\"FeatureCollection\",\"features\":[]}\n"
        );
    }

    #[test]
    fn numbers_are_written_at_a_fixed_precision() {
        assert_eq!(fixed(139.7, 7), "139.7000000");
        assert_eq!(fixed(-0.000_000_01, 7), "0.0000000");
        assert_eq!(fixed(-1.005, 2), "-1.00");
    }

    #[test]
    fn keys_order_missing_first_then_numbers_numerically() {
        let (two, ten, a) = (
            Value::Number(2.0),
            Value::Number(10.0),
            Value::String("a".into()),
        );
        let mut keys = [
            vec![Some(&a)],
            vec![Some(&ten)],
            vec![None],
            vec![Some(&two)],
        ];
        keys.sort_by(|x, y| compare_keys(x, y));
        assert_eq!(
            keys,
            [
                vec![None],
                vec![Some(&two)],
                vec![Some(&ten)],
                vec![Some(&a)]
            ]
        );
        assert_ne!(identity(&[None]), identity(&[Some(&Value::Null)]));
    }

    #[test]
    fn a_key_must_name_a_property() {
        let req = |key: Vec<&str>| FrameFeatures {
            nodes: vec![FrameFeatureNode::keyed("n", key)],
            coords: FeatureCoords::Px,
        };
        assert!(check_request(&req(vec!["id"])).is_ok());
        assert!(check_request(&req(vec![])).is_err());
        assert!(check_request(&req(vec!["id", ""])).is_err());
    }
}
