//! `ezu frame --features`: a features node's value, gathered over every tile
//! a print frame renders, written as one GeoJSON file.
//!
//! A print pipeline wants a machine-readable list of what is on the page —
//! every label, say, with what placement did to it — next to the image. The
//! node is evaluated per tile, so the same label shows up in each tile that
//! sees it; an optional key collapses those copies to one. The file must be
//! byte-identical across runs of the same input, so tiles are read in a fixed
//! order whatever order they finished in, and numbers are written at a fixed
//! precision.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

use clap::ValueEnum;
use ezu::core::frame::Frame;
use ezu::graph::{Graph, NodeIx, NodeObserver, OpaqueValue, PortKind, PortValue};
use ezu::paint::nodes::{FeatureGroup, FilteredFeatures};
use maplibre_expr::Value;

/// Decimal places for longitude and latitude: about 1 cm on the ground,
/// finer than any print pixel.
const LONLAT_DECIMALS: usize = 7;
/// Decimal places for output-image pixels.
const PX_DECIMALS: usize = 2;

/// Coordinate system of the written GeoJSON.
// The variants carry plain comments, not doc comments: clap would turn
// those into per-value help and switch the whole command's `--help` to
// its long layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FeatureCoords {
    // WGS84 longitude and latitude, as RFC 7946 GeoJSON expects.
    Lonlat,
    // Pixels of the output image: origin at its top-left corner, y down.
    Px,
}

/// One `--features NODE=PATH`, resolved against the graph.
pub(crate) struct Export {
    pub node: String,
    pub path: PathBuf,
    /// Where the node's value lands in each tile's captures.
    pub slot: usize,
    pub key: Option<Vec<String>>,
}

/// `NODE=PATH`.
pub(crate) fn parse_node_path(s: &str) -> Result<(String, PathBuf), String> {
    match s.split_once('=') {
        Some((node, path)) if !node.is_empty() && !path.is_empty() => {
            Ok((node.to_string(), PathBuf::from(path)))
        }
        _ => Err(format!("expected NODE=PATH, got `{s}`")),
    }
}

/// `NODE=PROP[,PROP…]`.
pub(crate) fn parse_node_key(s: &str) -> Result<(String, Vec<String>), String> {
    let bad = || format!("expected NODE=PROP[,PROP...], got `{s}`");
    let (node, props) = s.split_once('=').ok_or_else(bad)?;
    let props: Vec<String> = props.split(',').map(|p| p.trim().to_string()).collect();
    if node.is_empty() || props.iter().any(String::is_empty) {
        return Err(bad());
    }
    Ok((node.to_string(), props))
}

/// Check every requested node exists and yields features, and pair each with
/// its key. Done before anything renders, so a typo costs nothing.
pub(crate) fn resolve(
    graph: &Graph,
    features: &[(String, PathBuf)],
    keys: &[(String, Vec<String>)],
) -> Result<(Vec<Export>, Vec<NodeIx>), String> {
    let mut key_of: BTreeMap<&str, &Vec<String>> = BTreeMap::new();
    for (node, props) in keys {
        if !features.iter().any(|(n, _)| n == node) {
            return Err(format!(
                "--features-key names `{node}`, which no --features writes"
            ));
        }
        if key_of.insert(node, props).is_some() {
            return Err(format!("--features-key is given twice for `{node}`"));
        }
    }
    let mut nodes: Vec<NodeIx> = Vec::new();
    let mut exports = Vec::with_capacity(features.len());
    for (node, path) in features {
        let ix = graph
            .index_of(node)
            .ok_or_else(|| format!("--features: the style has no node `{node}`"))?;
        let kind = graph.output_kind(ix);
        if kind != PortKind::Features {
            return Err(format!(
                "--features: node `{node}` produces {kind}, not features"
            ));
        }
        let slot = match nodes.iter().position(|&n| n == ix) {
            Some(slot) => slot,
            None => {
                nodes.push(ix);
                nodes.len() - 1
            }
        };
        exports.push(Export {
            node: node.clone(),
            path: path.clone(),
            slot,
            key: key_of.get(node.as_str()).map(|k| (*k).clone()),
        });
    }
    Ok((exports, nodes))
}

/// Keeps the value of each watched node from one tile's render.
pub(crate) struct Capture {
    nodes: Vec<NodeIx>,
    values: Mutex<Vec<Option<OpaqueValue>>>,
}

impl Capture {
    pub(crate) fn new(nodes: Vec<NodeIx>) -> Self {
        let values = Mutex::new(vec![None; nodes.len()]);
        Capture { nodes, values }
    }

    /// The captured values, one per watched node in the order given to
    /// [`Capture::new`].
    pub(crate) fn take(&self) -> Vec<Option<OpaqueValue>> {
        std::mem::take(&mut *self.values.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

impl NodeObserver for Capture {
    fn on_node(&self, ix: NodeIx, value: &PortValue, _cache_hit: bool, _elapsed_us: u128) {
        let Some(slot) = self.nodes.iter().position(|&n| n == ix) else {
            return;
        };
        if let PortValue::Features(any) = value {
            self.values.lock().unwrap_or_else(|p| p.into_inner())[slot] = Some(any.clone());
        }
    }
}

/// The watched nodes' values from tile `(x, y)`.
pub(crate) struct TileCapture {
    pub x: u32,
    pub y: u32,
    pub values: Vec<Option<OpaqueValue>>,
}

/// One group, projected and ready to write.
struct Item<'a> {
    properties: &'a BTreeMap<String, Value>,
    geometry: String,
}

/// The GeoJSON FeatureCollection for `export`, from `tiles` (any order).
///
/// Tiles are read rows top to bottom, then left to right, and each tile's
/// groups in their own order. A group whose bounding box lies wholly outside
/// the frame is dropped; one that reaches into it is written whole, not
/// clipped. With a key, groups whose key properties are all equal are one
/// feature: the first in that reading order is kept, and the result is sorted
/// by the key. Without one, every group of every tile is written, so a
/// feature several tiles see appears once per tile, and a shape cut at tile
/// edges appears as its pieces.
pub(crate) fn feature_collection(
    frame: &Frame,
    tiles: &mut [TileCapture],
    export: &Export,
    coords: FeatureCoords,
) -> Result<String, String> {
    tiles.sort_by_key(|t| (t.y, t.x));
    let mut items: Vec<(Vec<Option<&Value>>, Item<'_>)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for tile in tiles.iter() {
        let Some(any) = tile.values.get(export.slot).and_then(Option::as_ref) else {
            continue;
        };
        let feats = any.downcast_ref::<FilteredFeatures>().ok_or_else(|| {
            format!(
                "--features: node `{}` produced features this command cannot read",
                export.node
            )
        })?;
        let project = Projection::new(frame, tile.x, tile.y, feats.extent, coords);
        for group in &feats.groups {
            let Some(bounds) = project.frame_bounds(group) else {
                continue;
            };
            if !bounds.reaches_into(frame) {
                continue;
            }
            let key: Vec<Option<&Value>> = match &export.key {
                Some(props) => props.iter().map(|p| group.properties.get(p)).collect(),
                None => Vec::new(),
            };
            if export.key.is_some() && !seen.insert(identity(&key)) {
                continue;
            }
            items.push((
                key,
                Item {
                    properties: &group.properties,
                    geometry: project.geometry(group),
                },
            ));
        }
    }
    if export.key.is_some() {
        // Stable, so equal keys keep the reading order.
        items.sort_by(|a, b| compare_keys(&a.0, &b.0));
    }

    let mut out = String::from(r#"{"type":"FeatureCollection","features":["#);
    for (i, (_, item)) in items.iter().enumerate() {
        out.push_str(if i == 0 { "\n" } else { ",\n" });
        let props: serde_json::Map<String, serde_json::Value> = item
            .properties
            .iter()
            .map(|(k, v)| (k.clone(), to_json(v)))
            .collect();
        let props = serde_json::to_string(&props).map_err(|e| e.to_string())?;
        let _ = write!(
            out,
            r#"{{"type":"Feature","geometry":{},"properties":{props}}}"#,
            item.geometry
        );
    }
    if !items.is_empty() {
        out.push('\n');
    }
    out.push_str("]}\n");
    Ok(out)
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
                s.push_str(&to_json(v).to_string());
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
            (Some(x), Some(y)) => to_json(x).to_string().cmp(&to_json(y).to_string()),
            _ => Ordering::Equal,
        });
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// A property value as JSON. A whole number is written without a fraction,
/// so a feature id reads `12`, not `12.0`; kinds JSON has no form for
/// (colours, formatted text) are written as their text.
fn to_json(v: &Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Number(n) => number(*n),
        Value::String(s) => J::String(s.clone()),
        Value::Array(a) => J::Array(a.iter().map(to_json).collect()),
        Value::Object(o) => J::Object(o.iter().map(|(k, v)| (k.clone(), to_json(v))).collect()),
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

/// Bounding box in output-image pixels.
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
    /// Tile origin in output-image pixels.
    origin: (f64, f64),
    /// Output-image pixels per geometry unit.
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

    /// `(u, v)` in geometry units → output-image pixels.
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
            FeatureCoords::Lonlat => (
                ezu::core::coord::world_x_to_lon((x + self.left) / self.world),
                ezu::core::coord::world_y_to_lat((y + self.top) / self.world),
                LONLAT_DECIMALS,
            ),
        };
        out.push('[');
        out.push_str(&fixed(a, decimals));
        out.push(',');
        out.push_str(&fixed(b, decimals));
        out.push(']');
    }

    fn positions(&self, out: &mut String, cs: impl IntoIterator<Item = (i32, i32)>) {
        out.push('[');
        for (i, c) in cs.into_iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            self.position(out, c);
        }
        out.push(']');
    }

    /// A ring closed and wound as RFC 7946 asks — exterior anticlockwise,
    /// holes clockwise, as seen on a north-up map — whatever order the
    /// node emitted it in.
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

    fn polygon(&self, out: &mut String, p: &ezu::features::Polygon) {
        out.push('[');
        self.ring(out, &p.exterior, true);
        for hole in &p.holes {
            out.push(',');
            self.ring(out, hole, false);
        }
        out.push(']');
    }

    /// Each kind present in `g`, as a single geometry when it has one item
    /// and a multi geometry otherwise.
    fn parts(&self, g: &FeatureGroup) -> Vec<String> {
        let mut parts = Vec::new();
        let mut s = String::new();
        match g.polygons.as_slice() {
            [] => {}
            [p] => {
                s.push_str(r#"{"type":"Polygon","coordinates":"#);
                self.polygon(&mut s, p);
                s.push('}');
                parts.push(std::mem::take(&mut s));
            }
            ps => {
                s.push_str(r#"{"type":"MultiPolygon","coordinates":["#);
                for (i, p) in ps.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    self.polygon(&mut s, p);
                }
                s.push_str("]}");
                parts.push(std::mem::take(&mut s));
            }
        }
        match g.lines.as_slice() {
            [] => {}
            [l] => {
                s.push_str(r#"{"type":"LineString","coordinates":"#);
                self.positions(&mut s, l.iter().copied());
                s.push('}');
                parts.push(std::mem::take(&mut s));
            }
            ls => {
                s.push_str(r#"{"type":"MultiLineString","coordinates":["#);
                for (i, l) in ls.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    self.positions(&mut s, l.iter().copied());
                }
                s.push_str("]}");
                parts.push(std::mem::take(&mut s));
            }
        }
        match g.points.as_slice() {
            [] => {}
            [p] => {
                s.push_str(r#"{"type":"Point","coordinates":"#);
                self.position(&mut s, *p);
                s.push('}');
                parts.push(std::mem::take(&mut s));
            }
            ps => {
                s.push_str(r#"{"type":"MultiPoint","coordinates":"#);
                self.positions(&mut s, ps.iter().copied());
                s.push('}');
                parts.push(std::mem::take(&mut s));
            }
        }
        parts
    }

    /// `g`'s geometry; a group mixing kinds becomes a GeometryCollection.
    fn geometry(&self, g: &FeatureGroup) -> String {
        let mut parts = self.parts(g);
        if parts.len() == 1 {
            return parts.pop().unwrap_or_default();
        }
        format!(
            r#"{{"type":"GeometryCollection","geometries":[{}]}}"#,
            parts.join(",")
        )
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
    use crate::{run_frame, CommonArgs, FrameCmd};
    use ezu::core::coord::{lat_to_world_y, lon_to_world_x, world_x_to_lon, world_y_to_lat};
    use ezu::core::frame::FrameRequest;
    use std::path::Path;

    /// 1:5000 at 96 dpi: tiles of ~188 px at z=17, and a 300×200 frame
    /// that straddles tiles on every side.
    fn request() -> FrameRequest {
        FrameRequest {
            center_lon: 139.70,
            center_lat: 35.66,
            scale: 5000.0,
            dpi: 96.0,
            width: 300,
            height: 200,
            zoom: None,
            style_tile_size: 256,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ezu-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn font_url() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ezu-core/tests/fonts/NotoSans-Regular.latin.ttf");
        format!("file:{}", path.display()).replace('\\', "/")
    }

    /// `lon,lat` of output-image pixel `(x, y)` of `frame`.
    fn lonlat_at(frame: &Frame, x: f64, y: f64) -> (f64, f64) {
        let world = frame.tile_size as f64 * (1u64 << frame.zoom) as f64;
        (
            world_x_to_lon((x + frame.left as f64) / world),
            world_y_to_lat((y + frame.top as f64) / world),
        )
    }

    /// Point features as GeoJSON, each `(name, lon, lat)`.
    fn points(pts: &[(&str, f64, f64)]) -> String {
        let feats: Vec<String> = pts
            .iter()
            .map(|(name, lon, lat)| {
                format!(
                    r#"{{ "type": "Feature", "properties": {{ "name": "{name}" }},
                         "geometry": {{ "type": "Point", "coordinates": [{lon}, {lat}] }} }}"#
                )
            })
            .collect();
        format!(
            r#"{{ "type": "FeatureCollection", "features": [{}] }}"#,
            feats.join(",")
        )
    }

    async fn frame(
        style: &str,
        dir: &Path,
        features: &[(&str, &Path)],
        key: Option<&str>,
        coords: FeatureCoords,
    ) -> Result<(), String> {
        let style_path = dir.join("style.json");
        std::fs::write(&style_path, style).unwrap();
        let req = request();
        run_frame(FrameCmd {
            common: CommonArgs {
                style: style_path.to_string_lossy().into_owned(),
                assets_dir: None,
                pmtiles: None,
                mvt: None,
                overzoom_levels: 4,
                params: Vec::new(),
                strict: false,
            },
            pages: None,
            center: Some((req.center_lon, req.center_lat)),
            scale: Some(req.scale),
            dpi: Some(req.dpi),
            size_mm: None,
            size_px: Some((req.width, req.height)),
            zoom: None,
            out: dir.join("page.png"),
            format: None,
            concurrency: Some(3),
            features: features
                .iter()
                .map(|(n, p)| (n.to_string(), p.to_path_buf()))
                .collect(),
            features_key: key
                .map(|k| vec![parse_node_key(k).unwrap()])
                .unwrap_or_default(),
            features_coords: coords,
        })
        .await
        .map_err(|e| e.to_string())
    }

    fn read(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn features(v: &serde_json::Value) -> &Vec<serde_json::Value> {
        v["features"].as_array().unwrap()
    }

    /// Black labels on white, with every `label-boxes` box exported.
    fn label_style(pts: &str) -> String {
        format!(
            r##"{{
              "name": "frame-labels",
              "tile-size": 256,
              "sources": {{
                "pts":  {{ "type": "geojson", "data": {pts} }},
                "body": {{ "type": "font", "url": "{font}" }}
              }},
              "nodes": {{
                "bg":     {{ "op": "solid", "color": "#ffffff" }},
                "feats":  {{ "op": "features", "source": "pts", "layer": "pts" }},
                "labels": {{ "op": "text-labels", "features": "@feats", "source": "pts", "layer": "pts",
                             "font": ["body"], "text": ["get", "name"], "size": 16,
                             "color": "#000000" }},
                "placed": {{ "op": "label-placement", "labels": ["@labels"] }},
                "boxes":  {{ "op": "label-boxes", "placement": "@placed", "include": "all" }},
                "text":   {{ "op": "text-draw", "labels": "@labels", "placement": "@placed" }},
                "out":    {{ "op": "stack", "layers": ["@bg", "@text"] }}
              }},
              "output": "@out"
            }}"##,
            font = font_url(),
        )
    }

    /// Labels straddling a tile edge inside the frame are listed once each
    /// with the label-boxes key, the file is the same on every run, and the
    /// text boxes sit where the text is drawn.
    #[tokio::test(flavor = "multi_thread")]
    async fn label_boxes_across_tile_edges_are_listed_once() {
        let dir = scratch("frame-labels");
        let fr = Frame::compute(&request()).unwrap();
        assert!(fr.tiles_x.len() >= 3);
        // The first tile edge inside the frame, and a row in its middle.
        let (edge, _) = fr.tile_offset(fr.tiles_x.start + 1, fr.tiles_y.start);
        let edge = edge as f64;
        let pts = [("Alpha", edge - 3.0, 60.0), ("Beta", edge + 4.0, 140.0)];
        let geo: Vec<(&str, f64, f64)> = pts
            .iter()
            .map(|&(n, x, y)| {
                let (lon, lat) = lonlat_at(&fr, x, y);
                (n, lon, lat)
            })
            .collect();
        let style = label_style(&points(&geo));

        let (a, b, raw) = (dir.join("a.json"), dir.join("b.json"), dir.join("raw.json"));
        let key = Some("boxes=label-id,part");
        frame(&style, &dir, &[("boxes", &a)], key, FeatureCoords::Px)
            .await
            .unwrap();
        frame(&style, &dir, &[("boxes", &b)], key, FeatureCoords::Px)
            .await
            .unwrap();
        frame(&style, &dir, &[("boxes", &raw)], None, FeatureCoords::Px)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());

        let key_of = |f: &serde_json::Value| {
            let p = &f["properties"];
            (
                p["label-id"].as_i64().unwrap(),
                p["part"].as_str().unwrap().to_string(),
            )
        };
        let doc = read(&a);
        let keyed: Vec<_> = features(&doc).iter().map(key_of).collect();
        let mut sorted = keyed.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(keyed, sorted, "each (label-id, part) once, in key order");
        // Both tiles beside the edge saw both labels: without the key the
        // copies are all there.
        let raw_doc = read(&raw);
        assert!(features(&raw_doc).len() > features(&doc).len());

        let image = tiny_skia::Pixmap::load_png(dir.join("page.png")).unwrap();
        let dark = |x: u32, y: u32| image.pixel(x, y).is_some_and(|p| p.red() < 128);
        for (name, ax, ay) in pts {
            let text = features(&doc)
                .iter()
                .find(|f| f["properties"]["name"] == name && f["properties"]["part"] == "text")
                .unwrap_or_else(|| panic!("a text box for {name}"));
            assert_eq!(text["properties"]["placed"], true);
            let ring = text["geometry"]["coordinates"][0].as_array().unwrap();
            let xs: Vec<f64> = ring.iter().map(|c| c[0].as_f64().unwrap()).collect();
            let ys: Vec<f64> = ring.iter().map(|c| c[1].as_f64().unwrap()).collect();
            let fold = |v: &[f64], f: fn(f64, f64) -> f64, init| v.iter().copied().fold(init, f);
            let (x0, x1) = (fold(&xs, f64::min, f64::MAX), fold(&xs, f64::max, f64::MIN));
            let (y0, y1) = (fold(&ys, f64::min, f64::MAX), fold(&ys, f64::max, f64::MIN));
            // Centred on the anchor, and across the tile edge.
            assert!((0.5 * (x0 + x1) - ax).abs() < 2.0, "{name}: {x0}..{x1}");
            assert!((0.5 * (y0 + y1) - ay).abs() < 3.0, "{name}: {y0}..{y1}");
            assert!(x0 < edge && x1 > edge, "{name} straddles the edge");
            // The text's ink lies inside its box, and there is ink.
            let mut ink = 0;
            for y in (y0 - 6.0) as u32..(y1 + 6.0) as u32 {
                for x in (x0 - 6.0) as u32..(x1 + 6.0) as u32 {
                    if dark(x, y) {
                        ink += 1;
                        let inside = (x as f64) >= x0 - 1.0
                            && (x as f64) <= x1 + 1.0
                            && (y as f64) >= y0 - 1.0
                            && (y as f64) <= y1 + 1.0;
                        assert!(inside, "{name}: ink at ({x}, {y}) outside its box");
                    }
                }
            }
            assert!(ink > 20, "{name}: drawn");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A point written as lon/lat comes back where it was declared, and as
    /// px where the frame puts it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_point_round_trips_through_lonlat_and_px() {
        let dir = scratch("frame-point");
        let fr = Frame::compute(&request()).unwrap();
        let (lon, lat) = (139.7012345, 35.6598765);
        let style = format!(
            r##"{{
              "name": "frame-point",
              "tile-size": 256,
              "sources": {{ "pts": {{ "type": "geojson", "data": {pts} }} }},
              "nodes": {{
                "feats": {{ "op": "features", "source": "pts", "layer": "pts" }},
                "dots":  {{ "op": "circles", "features": "@feats", "radius": 3, "color": "#000000" }}
              }},
              "output": "@dots"
            }}"##,
            pts = points(&[("p", lon, lat)]),
        );
        let (geo, px) = (dir.join("geo.json"), dir.join("px.json"));
        let key = Some("feats=name");
        frame(&style, &dir, &[("feats", &geo)], key, FeatureCoords::Lonlat)
            .await
            .unwrap();
        frame(&style, &dir, &[("feats", &px)], key, FeatureCoords::Px)
            .await
            .unwrap();

        let doc = read(&geo);
        let [f] = features(&doc).as_slice() else {
            panic!("one feature: {doc}");
        };
        assert_eq!(f["geometry"]["type"], "Point");
        let c = &f["geometry"]["coordinates"];
        // Tile geometry is quantised to 1/4096 of a ~188 px tile.
        assert!((c[0].as_f64().unwrap() - lon).abs() < 2e-6, "{c}");
        assert!((c[1].as_f64().unwrap() - lat).abs() < 2e-6, "{c}");

        let doc = read(&px);
        let c = &features(&doc)[0]["geometry"]["coordinates"];
        let world = fr.tile_size as f64 * (1u64 << fr.zoom) as f64;
        let want = (
            lon_to_world_x(lon) * world - fr.left as f64,
            lat_to_world_y(lat) * world - fr.top as f64,
        );
        assert!(
            (c[0].as_f64().unwrap() - want.0).abs() < 0.1,
            "{c} vs {want:?}"
        );
        assert!(
            (c[1].as_f64().unwrap() - want.1).abs() < 0.1,
            "{c} vs {want:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn only_features_nodes_can_be_written() {
        let dir = scratch("frame-features-errors");
        let style = label_style(&points(&[]));
        let out = dir.join("x.json");
        let err = frame(&style, &dir, &[("nope", &out)], None, FeatureCoords::Lonlat)
            .await
            .unwrap_err();
        assert!(err.contains("no node `nope`"), "{err}");
        let err = frame(&style, &dir, &[("bg", &out)], None, FeatureCoords::Lonlat)
            .await
            .unwrap_err();
        assert!(err.contains("`bg` produces raster"), "{err}");
        let err = frame(
            &style,
            &dir,
            &[("boxes", &out)],
            Some("feats=name"),
            FeatureCoords::Lonlat,
        )
        .await
        .unwrap_err();
        assert!(err.contains("no --features writes"), "{err}");
        assert!(!out.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn flags_parse() {
        assert_eq!(
            parse_node_path("boxes=out/labels.geojson"),
            Ok(("boxes".into(), PathBuf::from("out/labels.geojson")))
        );
        assert!(parse_node_path("boxes").is_err());
        assert!(parse_node_path("=x.geojson").is_err());
        assert_eq!(
            parse_node_key("boxes=label-id,part"),
            Ok(("boxes".into(), vec!["label-id".into(), "part".into()]))
        );
        assert!(parse_node_key("boxes=label-id,").is_err());
    }

    #[test]
    fn numbers_are_written_at_a_fixed_precision() {
        assert_eq!(fixed(139.7, 7), "139.7000000");
        assert_eq!(fixed(-0.000_000_01, 7), "0.0000000");
        assert_eq!(fixed(-1.005, 2), "-1.00");
        assert_eq!(to_json(&Value::Number(12.0)).to_string(), "12");
        assert_eq!(to_json(&Value::Number(0.5)).to_string(), "0.5");
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
}
