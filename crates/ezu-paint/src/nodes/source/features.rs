//! `features` — `() -> Features`. Resolves a host-bound feature layer
//! via the unified [`AssetLoader`](ezu_graph::AssetLoader), applies an
//! optional `filter-expr` (a MapLibre filter expression) and
//! `min-zoom-field`, and emits the surviving features as a
//! [`FilteredFeatures`].
//!
//! Style fields: `source` (optional, matches an `mvt`/`pmtiles`/
//! `geojson` entry in the document's `sources` block; omitting it falls
//! back to the single such entry and warns) + `layer` (the MVT layer
//! name; for `geojson` the host binds the layer under the source's own
//! name). The op looks up `<source>.<layer>` on the host's AssetLoader.
//! A missing binding is treated as "no features for this tile" and
//! yields an empty result.
//!
//! ## Stitching across tiles
//!
//! A vector tile carries only a thin buffer of geometry past its edge
//! (16 px on a 512 px protomaps tile), and polygons are cut where that
//! buffer ends. Anything that reaches further than the buffer — a wide
//! stroke along a coastline, a large `buffer`, a closing — sees the
//! data stop short, and shows a seam or misses a coast that lies just
//! beyond. Setting `stitch` joins the layer to its eight neighbouring
//! tiles first, so geometry runs on as far as the graph needs it:
//!
//! - `"merge"`: polygons whose properties are identical are unioned
//!   into one, so no edge remains where the tiles met. Use it when the
//!   outline matters (strokes, boundaries, buffers).
//! - `"pieces"`: every piece stays as its own tile cut it. Cheaper, and
//!   looks the same when the layer is only filled; also the choice when
//!   you union further down the graph yourself.
//!
//! The plane is split into a 3×3 grid of cells around this tile, and
//! each cell takes its geometry from the tile it belongs to, so the
//! buffers where tiles overlap are not drawn twice. A neighbour the host
//! could not provide leaves its cell to this tile's own buffer data.
//! Stitching costs fetching and decoding up to eight more tiles, for
//! this layer only.

use std::collections::HashMap;
use std::sync::Arc;

use ezu_features::ops::boolean::polygon_union_all;
use ezu_features::ops::clip::{clip_line_to_rect, clip_polygon_to_rect, ClipRect, Span};
use ezu_graph::{
    Asset, AssetError, BuiltNode, CoordSpace, EvalCtx, EvalError, FactoryCtx, FactoryError, Node,
    NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use crate::nodes::common::{
    cull_groups, features_value, features_value_culled, neighbor_feature_groups,
    read_optional_string, read_optional_zoom, resolve_source, FeatureGroup,
};
use crate::render::{collect_groups, SharedLayer};

/// How a stitched layer treats the pieces its tiles were cut into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stitch {
    /// Union polygons with identical properties across the seams.
    Merge,
    /// Keep each tile's pieces as they are.
    Pieces,
}

struct FeaturesNode {
    name: String,
    /// A MapLibre filter expression, compiled once and evaluated per feature.
    filter_expr: Option<maplibre_expr::Expr>,
    /// The raw `filter-expr` JSON text, kept only for a stable cache hash.
    filter_expr_src: Option<String>,
    min_zoom_field: Option<String>,
    min_zoom: Option<u8>,
    max_zoom: Option<u8>,
    /// Join the neighbouring tiles' copies of the layer (`None`: off).
    stitch: Option<Stitch>,
}

impl Node for FeaturesNode {
    fn op_name(&self) -> &'static str {
        "features"
    }
    fn inputs(&self) -> &[PortSpec] {
        &[]
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Features
    }
    fn coord_space(&self) -> CoordSpace {
        // Features live in tile-local coordinates ([0, extent]).
        CoordSpace::Tile
    }
    fn asset_inputs(&self) -> Vec<String> {
        let mut names = vec![self.name.clone()];
        if self.stitch.is_some() {
            names.extend(ezu_graph::neighbor_bindings(&self.name));
        }
        names
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        _inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let z = ctx.tile.z;
        // Style-level zoom gate: outside the [min_zoom, max_zoom] band,
        // skip the asset lookup entirely and emit an empty layer.
        if self.min_zoom.is_some_and(|mn| z < mn) || self.max_zoom.is_some_and(|mx| z > mx) {
            return Ok(features_value(0, vec![]));
        }
        let asset = match ctx.assets.load(&self.name) {
            Ok(a) => a,
            // No binding for this tile -> emit an empty layer.
            Err(AssetError::NotFound(_)) => return Ok(features_value(0, vec![])),
            Err(e) => return Err(EvalError::Asset(e)),
        };
        let Asset::Features(opq) = asset else {
            return Err(EvalError::Other(format!(
                "asset `{}` is not a feature layer",
                self.name
            )));
        };
        let shared = opq.downcast::<SharedLayer>().map_err(|_| {
            EvalError::Other(format!("`{}` payload is not a feature layer", self.name))
        })?;
        let fe = self.filter_expr.as_ref();
        let extent = shared.layer.extent;
        let mut groups = collect_groups(&shared, fe, &self.min_zoom_field, z);
        if let Some(mode) = self.stitch {
            let e = extent.max(1) as i64;
            let neighbors =
                neighbor_feature_groups(ctx, &self.name, e, fe, &self.min_zoom_field, z);
            // With no neighbour bound there is nothing to join, and this
            // tile's own data is exactly what an unstitched layer emits.
            if !neighbors.is_empty() {
                groups = stitch_groups(groups, neighbors, e as i32);
                if mode == Stitch::Merge {
                    // Drop out-of-reach pieces before paying for the union.
                    groups = merge_groups(cull_groups(ctx, extent, groups));
                }
            }
        }
        Ok(features_value_culled(ctx, extent, groups))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"features");
        h.update(self.name.as_bytes());
        if let Some(s) = &self.filter_expr_src {
            h.update(b"fexpr");
            h.update(s.as_bytes());
        }
        if let Some(s) = &self.min_zoom_field {
            h.update(s.as_bytes());
        }
        if let Some(z) = self.min_zoom {
            h.update(b"minz");
            h.update(&[z]);
        }
        if let Some(z) = self.max_zoom {
            h.update(b"maxz");
            h.update(&[z]);
        }
        if let Some(mode) = self.stitch {
            h.update(b"stitch");
            h.update(match mode {
                Stitch::Merge => b"merge" as &[u8],
                Stitch::Pieces => b"pieces",
            });
        }
    }
}

/// How far out the outer cells reach — "unbounded", kept finite for the
/// integer geometry and far beyond any tile's buffer.
const FAR: i32 = 1 << 30;

/// One axis of cell `c` ∈ `-1..=1` for a tile of extent `e`: `(-∞, 0)`,
/// `[0, e]` or `(e, +∞)`. The centre is closed and the outer cells open
/// towards it, so a point on a cell line belongs to the centre.
fn cell_span(c: i64, e: i32) -> Span {
    match c {
        -1 => Span {
            lo: -FAR,
            hi: 0,
            lo_open: false,
            hi_open: true,
        },
        0 => Span::closed(0, e),
        _ => Span {
            lo: e,
            hi: FAR,
            lo_open: true,
            hi_open: false,
        },
    }
}

fn cell(cx: i64, cy: i64, e: i32) -> ClipRect {
    ClipRect {
        x: cell_span(cx, e),
        y: cell_span(cy, e),
    }
}

/// Append the part of `g` inside `r` to `out`, after moving `g` by `off`.
fn clip_into(out: &mut FeatureGroup, g: &FeatureGroup, off: (i32, i32), r: &ClipRect) {
    let mv = |&(x, y): &(i32, i32)| (x + off.0, y + off.1);
    for p in &g.polygons {
        let moved;
        let p = if off == (0, 0) {
            p
        } else {
            moved = ezu_features::Polygon {
                exterior: p.exterior.iter().map(mv).collect(),
                holes: p.holes.iter().map(|h| h.iter().map(mv).collect()).collect(),
            };
            &moved
        };
        out.polygons.extend(clip_polygon_to_rect(p, r));
    }
    for l in &g.lines {
        let l: Vec<(i32, i32)> = l.iter().map(mv).collect();
        out.lines.extend(clip_line_to_rect(&l, r));
    }
    out.points
        .extend(g.points.iter().map(mv).filter(|&p| r.contains(p)));
}

/// Join this tile's groups (`centre`) with its neighbours' (each in its
/// own `[0, e]` frame at tile offset `(dx, dy)`), all in this tile's
/// frame. Each of the 3×3 cells takes its geometry from the tile it
/// belongs to when that tile is present, else from the centre's buffer.
fn stitch_groups(
    centre: Vec<FeatureGroup>,
    neighbors: Vec<(Vec<FeatureGroup>, i64, i64)>,
    e: i32,
) -> Vec<FeatureGroup> {
    let bound: Vec<(i64, i64)> = neighbors.iter().map(|&(_, dx, dy)| (dx, dy)).collect();
    let centre_cells: Vec<ClipRect> = (-1..=1)
        .flat_map(|cy| (-1..=1).map(move |cx| (cx, cy)))
        .filter(|&c| c == (0, 0) || !bound.contains(&c))
        .map(|(cx, cy)| cell(cx, cy, e))
        .collect();
    let empty_like = |g: &FeatureGroup| FeatureGroup {
        properties: Arc::clone(&g.properties),
        polygons: Vec::new(),
        lines: Vec::new(),
        points: Vec::new(),
    };
    let is_empty =
        |g: &FeatureGroup| g.polygons.is_empty() && g.lines.is_empty() && g.points.is_empty();

    let mut out = Vec::new();
    for g in &centre {
        let mut piece = empty_like(g);
        for r in &centre_cells {
            clip_into(&mut piece, g, (0, 0), r);
        }
        if !is_empty(&piece) {
            out.push(piece);
        }
    }
    for (groups, dx, dy) in &neighbors {
        let r = cell(*dx, *dy, e);
        let off = (*dx as i32 * e, *dy as i32 * e);
        for g in groups {
            let mut piece = empty_like(g);
            clip_into(&mut piece, g, off, &r);
            if !is_empty(&piece) {
                out.push(piece);
            }
        }
    }
    out
}

/// Fold groups with identical properties into one, in first-seen order,
/// and union each one's polygons so no edge is left where pieces meet.
/// Lines and points are concatenated.
fn merge_groups(groups: Vec<FeatureGroup>) -> Vec<FeatureGroup> {
    let mut out: Vec<FeatureGroup> = Vec::new();
    let mut by_key: HashMap<u64, Vec<usize>> = HashMap::new();
    for g in groups {
        let slot = by_key.entry(properties_key(&g.properties)).or_default();
        let same = slot.iter().copied().find(|&i| {
            Arc::ptr_eq(&out[i].properties, &g.properties) || out[i].properties == g.properties
        });
        match same {
            Some(i) => {
                let o = &mut out[i];
                o.polygons.extend(g.polygons);
                o.lines.extend(g.lines);
                o.points.extend(g.points);
            }
            None => {
                slot.push(out.len());
                out.push(g);
            }
        }
    }
    for g in &mut out {
        if g.polygons.len() > 1 {
            g.polygons = polygon_union_all(&g.polygons);
        }
    }
    out
}

/// A hash that equal property maps share (equality still decides).
fn properties_key(props: &std::collections::BTreeMap<String, maplibre_expr::Value>) -> u64 {
    use maplibre_expr::Value as V;
    let mut h = Xxh3::new();
    for (k, v) in props {
        h.update(k.as_bytes());
        match v {
            V::Null => h.update(b"n"),
            V::Bool(b) => h.update(&[b'b', *b as u8]),
            // `0.0 == -0.0`, so both must hash alike.
            V::Number(n) => h.update(
                &(if *n == 0.0 { 0.0f64 } else { *n })
                    .to_bits()
                    .to_le_bytes(),
            ),
            V::String(s) => {
                h.update(b"s");
                h.update(s.as_bytes());
            }
            // Never decoded from a tile; left to the equality check.
            _ => h.update(b"?"),
        }
        h.update(b"\0");
    }
    h.digest()
}

pub(super) struct FeaturesFactory;
impl NodeFactory for FeaturesFactory {
    fn op_name(&self) -> &'static str {
        "features"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let layer = fields
            .get("layer")
            .and_then(Value::as_str)
            .ok_or_else(|| FactoryError::MissingField("layer".into()))?
            .to_string();
        let source = resolve_feature_source(fields, ctx)?;
        let name = format!("{source}.{layer}");
        // `filter-expr`: a raw MapLibre filter expression, compiled once.
        let (filter_expr, filter_expr_src) = match fields.get("filter-expr") {
            Some(v) => {
                let expr = maplibre_expr::parse(v).map_err(|e| FactoryError::BadField {
                    field: "filter-expr".into(),
                    msg: e.to_string(),
                })?;
                (Some(expr), Some(v.to_string()))
            }
            None => (None, None),
        };
        let min_zoom_field = read_optional_string(fields, "min-zoom-field")?;
        let min_zoom = read_optional_zoom(fields, "min-zoom")?;
        let max_zoom = read_optional_zoom(fields, "max-zoom")?;
        let stitch = match read_optional_string(fields, "stitch")?.as_deref() {
            None => None,
            Some("merge") => Some(Stitch::Merge),
            Some("pieces") => Some(Stitch::Pieces),
            Some(other) => {
                return Err(FactoryError::BadField {
                    field: "stitch".into(),
                    msg: format!("unknown stitch '{other}', expected merge/pieces"),
                })
            }
        };
        Ok(BuiltNode {
            node: Box::new(FeaturesNode {
                name,
                filter_expr,
                filter_expr_src,
                min_zoom_field,
                min_zoom,
                max_zoom,
                stitch,
            }),
            connections: vec![],
        })
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "description": "Sample features from a host-bound feature layer. `source` names an `mvt`/`pmtiles`/`geojson` entry in the document's `sources` block (omitting it resolves to the only such source, with a build warning); `layer` selects a layer within that source (for `geojson`, the layer name equals the source name).",
            "properties": {
                "source": { "type": "string",
                            "description": "Name of an `mvt`, `pmtiles`, or `geojson` entry in the document's `sources`. Omitting it resolves to the only such source and warns at build time." },
                "layer": { "type": "string",
                           "description": "Vector tile layer name within `source` (e.g. `earth`, `roads`)." },
                "filter-expr": {
                    "description": "A MapLibre filter expression (JSON array, e.g. [\"all\", [\"==\", [\"get\", \"class\"], \"primary\"], [\"has\", \"name\"]]), evaluated per feature. A feature passes only when the expression is truthy. Supports the full expression language (any/has/comparisons/geometry-type)."
                },
                "min-zoom-field": { "type": "string",
                                    "description": "Per-feature property name carrying its data-side `min_zoom`. Features with `<field> > z` are dropped." },
                "min-zoom": { "type": "integer", "minimum": 0, "maximum": 24,
                              "description": "Style-level minimum zoom. Below this zoom the node emits an empty layer (the asset is not even loaded)." },
                "max-zoom": { "type": "integer", "minimum": 0, "maximum": 24,
                              "description": "Style-level maximum zoom. Above this zoom the node emits an empty layer." },
                "stitch": { "type": "string", "enum": ["merge", "pieces"],
                            "description": "Join the layer to its 8 neighbouring tiles, for effects that reach further than the tile's thin buffer: wide strokes along outlines, large buffers, closings. Each tile contributes only the area it owns, so nothing is drawn twice. `merge` unions polygons with identical properties so no edge remains where tiles meet; `pieces` keeps every piece as its tile cut it (cheaper, and identical when the layer is only filled). Costs fetching and decoding up to 8 more tiles for this layer only; a neighbour the host cannot provide falls back to this tile's own buffer data. Omit to read this tile alone." },
            },
            "required": ["layer"],
        })
    }
}

/// Whether a source declaration can feed a `features` node: vector
/// tiles (`mvt`/`pmtiles`) or GeoJSON (the host projects it into each
/// tile and binds it as one feature layer).
fn is_feature_source(decl: &ezu_style::SourceDecl) -> bool {
    matches!(
        decl,
        ezu_style::SourceDecl::Mvt(_)
            | ezu_style::SourceDecl::Pmtiles(_)
            | ezu_style::SourceDecl::GeoJson(_)
    )
}

/// Resolve the `source` field for ops that target a feature source.
/// When omitted, defaults to the document's single `mvt`/`pmtiles`/
/// `geojson` source — with a build warning, since the style then leans
/// on something it does not state. Errors if `source` is omitted and
/// the document has zero or multiple such sources, or if a named
/// source doesn't exist / isn't a feature source.
fn resolve_feature_source(
    fields: &serde_json::Map<String, Value>,
    ctx: &FactoryCtx<'_>,
) -> Result<String, FactoryError> {
    resolve_source(fields, ctx, "`mvt`/`pmtiles`/`geojson`", is_feature_source)
}

ezu_graph::submit_node!(FeaturesFactory);

#[cfg(test)]
mod tests {
    use super::*;
    use ezu_features::Polygon;
    use std::collections::BTreeMap;

    const E: i32 = 4096;
    /// The protomaps tile buffer, in extent units.
    const BUF: i32 = 128;

    fn props(kind: &str) -> Arc<BTreeMap<String, maplibre_expr::Value>> {
        let mut m = BTreeMap::new();
        m.insert("kind".into(), maplibre_expr::Value::String(kind.into()));
        Arc::new(m)
    }

    fn group(kind: &str) -> FeatureGroup {
        FeatureGroup {
            properties: props(kind),
            polygons: vec![],
            lines: vec![],
            points: vec![],
        }
    }

    fn square(x0: i32, y0: i32, x1: i32, y1: i32) -> Polygon {
        Polygon {
            exterior: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)],
            holes: vec![],
        }
    }

    fn x_range(p: &Polygon) -> (i32, i32) {
        let xs = p.exterior.iter().map(|&(x, _)| x);
        (xs.clone().min().unwrap(), xs.max().unwrap())
    }

    /// One polygon running from the centre into the east neighbour, as the
    /// two tiles cut it: each copy reaches `BUF` past its own edge.
    #[allow(clippy::type_complexity)]
    fn across_east_seam() -> (Vec<FeatureGroup>, Vec<(Vec<FeatureGroup>, i64, i64)>) {
        let mut c = group("water");
        c.polygons.push(square(3000, 1000, E + BUF, 2000));
        let mut n = group("water");
        n.polygons.push(square(-BUF, 1000, 1000, 2000));
        (vec![c], vec![(vec![n], 1, 0)])
    }

    #[test]
    fn pieces_split_at_the_seam_without_overlap() {
        let (c, n) = across_east_seam();
        let out = stitch_groups(c, n, E);
        let ranges: Vec<_> = out.iter().flat_map(|g| &g.polygons).map(x_range).collect();
        assert_eq!(ranges, vec![(3000, E), (E, E + 1000)]);
    }

    #[test]
    fn merge_leaves_one_polygon_and_no_seam_edge() {
        let (c, n) = across_east_seam();
        let out = merge_groups(stitch_groups(c, n, E));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].polygons.len(), 1, "{:?}", out[0].polygons);
        let p = &out[0].polygons[0];
        assert_eq!(x_range(p), (3000, E + 1000));
        assert!(
            p.exterior.iter().all(|&(x, _)| x != E),
            "a vertex stayed on the seam: {:?}",
            p.exterior
        );
    }

    #[test]
    fn merge_keeps_different_properties_apart() {
        let (mut c, mut n) = across_east_seam();
        n[0].0[0].properties = props("land");
        c[0].polygons.push(square(100, 100, 200, 200));
        let out = merge_groups(stitch_groups(c, n, E));
        assert_eq!(out.len(), 2);
        // The centre group's two separate pieces stay two polygons.
        assert_eq!(out[0].polygons.len(), 2);
    }

    #[test]
    fn a_missing_neighbour_leaves_the_centre_buffer() {
        // Only the north neighbour is bound: the east cell still belongs
        // to the centre, whose buffer reaches past the edge.
        let (c, _) = across_east_seam();
        let north = vec![(vec![group("water")], 0, -1)];
        let out = stitch_groups(c, north, E);
        let max_x = out
            .iter()
            .flat_map(|g| &g.polygons)
            .map(|p| x_range(p).1)
            .max();
        assert_eq!(max_x, Some(E + BUF));
    }

    #[test]
    fn a_line_into_a_neighbour_is_not_doubled() {
        let mut c = group("coast");
        c.lines.push(vec![(3000, 2000), (E + BUF, 2000)]);
        let mut n = group("coast");
        n.lines.push(vec![(-BUF, 2000), (1000, 2000)]);
        let out = stitch_groups(vec![c], vec![(vec![n], 1, 0)], E);
        let lines: Vec<_> = out.iter().flat_map(|g| g.lines.clone()).collect();
        assert_eq!(
            lines,
            vec![
                vec![(3000, 2000), (E, 2000)],
                vec![(E, 2000), (E + 1000, 2000)]
            ]
        );
    }

    #[test]
    fn a_point_on_a_cell_line_is_owned_once() {
        // The same world point on the east edge, in the centre's frame and
        // in the east neighbour's; and one on the west edge likewise.
        let mut c = group("poi");
        c.points.extend([(E, 100), (0, 300)]);
        let mut east = group("poi");
        east.points.push((0, 100));
        let mut west = group("poi");
        west.points.push((E, 300));
        let out = stitch_groups(vec![c], vec![(vec![west], -1, 0), (vec![east], 1, 0)], E);
        let mut pts: Vec<_> = out.iter().flat_map(|g| g.points.clone()).collect();
        pts.sort();
        assert_eq!(pts, vec![(0, 300), (E, 100)]);
    }

    #[test]
    fn equal_properties_share_a_key() {
        let mut a = BTreeMap::new();
        a.insert("n".to_string(), maplibre_expr::Value::Number(0.0));
        let mut b = BTreeMap::new();
        b.insert("n".to_string(), maplibre_expr::Value::Number(-0.0));
        assert_eq!(a, b);
        assert_eq!(properties_key(&a), properties_key(&b));
    }
}
