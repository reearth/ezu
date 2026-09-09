//! `ezu graph --tile Z/X/Y`: the node DAG with each node's own
//! intermediate tile drawn onto it.
//!
//! One render produces every picture. The evaluator already walks the
//! whole graph to reach the output, so a [`NodeObserver`] watching that
//! single pass sees each node's value exactly once — no node is
//! re-evaluated, and nothing is rendered that the tile did not already
//! need. Each value is encoded the moment it arrives and the pixels are
//! dropped, so collecting a 78-node graph costs one tile's worth of
//! residency plus the thumbnails.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use base64::Engine;
use ezu::graph::{
    describe_value, CanvasInfo, Graph, NodeIx, NodeObserver, PortKind, PortValue, RasterBuf,
    ScalarValue,
};
use ezu::paint::host::{crop_to_png_scaled, PngCompression};
use ezu::style::{Document, SourceDecl};
use serde_json::{json, Map, Value as Json};

/// The viewer page. `__EZU_GRAPH__` is replaced with the payload built
/// by [`Collected::into_payload`].
const VIEWER_HTML: &str = include_str!("graph_view.html");

/// What one node contributed to the render.
struct Preview {
    /// `describe_value` — the same one-liner the `-v` node trace prints.
    summary: String,
    /// Extra facts the summary has no room for: feature counts, a
    /// scalar's value, a field's range.
    detail: Option<String>,
    ms: f64,
    cached: bool,
    /// A raster that came back fully transparent. Common enough (most
    /// layers match nothing on most tiles) that it is worth saying so
    /// instead of embedding an empty picture per node.
    empty: bool,
    png: Option<Vec<u8>>,
}

/// Watches one render and keeps a thumbnail plus a summary per node.
pub struct Collector {
    canvas: CanvasInfo,
    image_size: u32,
    seen: Mutex<HashMap<NodeIx, Preview>>,
}

/// A finished [`Collector`], unwrapped from its lock.
pub struct Collected {
    canvas: CanvasInfo,
    previews: HashMap<NodeIx, Preview>,
}

impl Collector {
    /// `image_size` caps a thumbnail's long edge; `0` keeps full size.
    pub fn new(canvas: CanvasInfo, image_size: u32) -> Self {
        Self {
            canvas,
            image_size,
            seen: Mutex::new(HashMap::new()),
        }
    }

    pub fn finish(self) -> Collected {
        Collected {
            canvas: self.canvas,
            previews: self.seen.into_inner().unwrap_or_else(|p| p.into_inner()),
        }
    }

    fn preview(&self, value: &PortValue, cache_hit: bool, elapsed_us: u128) -> Preview {
        let mut detail = None;
        let mut empty = false;
        let png = match value {
            PortValue::Raster(r) => {
                if RasterBuf::is_interned_blank(r) {
                    empty = true;
                    None
                } else {
                    self.encode(r, self.canvas.tile_w, self.canvas.tile_h, self.canvas.pad)
                }
            }
            // A sprite is the asset at its own size, with no canvas pad
            // to crop away.
            PortValue::Sprite(s) => {
                detail = Some(format!("{}×{} px, source size", s.width, s.height));
                self.encode(s, s.width, s.height, 0)
            }
            PortValue::ScalarField(f) => {
                let (gray, range) = scalar_field_to_gray(f);
                detail = Some(range);
                // Fields are canvas-shaped like rasters, so they crop the
                // same way — unless a producer hands back something else,
                // in which case show it whole.
                let padded = (self.canvas.padded_w(), self.canvas.padded_h());
                if (gray.width, gray.height) == padded {
                    self.encode(
                        &gray,
                        self.canvas.tile_w,
                        self.canvas.tile_h,
                        self.canvas.pad,
                    )
                } else {
                    self.encode(&gray, gray.width, gray.height, 0)
                }
            }
            PortValue::Features(o) => {
                detail = o
                    .downcast_ref::<ezu::paint::nodes::FilteredFeatures>()
                    .map(describe_features);
                None
            }
            PortValue::Scalar(s) => {
                detail = Some(match s {
                    ScalarValue::Color(c) => format!(
                        "#{:02x}{:02x}{:02x}{:02x}",
                        (c[0] * 255.0).round() as u8,
                        (c[1] * 255.0).round() as u8,
                        (c[2] * 255.0).round() as u8,
                        (c[3] * 255.0).round() as u8,
                    ),
                    ScalarValue::Number(n) => format!("{n}"),
                    ScalarValue::Bool(b) => format!("{b}"),
                });
                None
            }
            PortValue::Brush(_) | PortValue::Labels(_) => None,
        };
        Preview {
            summary: describe_value(value),
            detail,
            ms: elapsed_us as f64 / 1000.0,
            cached: cache_hit,
            empty,
            png,
        }
    }

    fn encode(&self, buf: &RasterBuf, crop_w: u32, crop_h: u32, pad: u32) -> Option<Vec<u8>> {
        // `Fast` here on purpose: these are throwaway debug pictures and
        // there are as many of them as there are nodes.
        crop_to_png_scaled(
            buf,
            crop_w,
            crop_h,
            pad,
            self.image_size,
            PngCompression::Fast,
        )
        .ok()
    }
}

impl NodeObserver for Collector {
    fn on_node(&self, ix: NodeIx, value: &PortValue, cache_hit: bool, elapsed_us: u128) {
        let preview = self.preview(value, cache_hit, elapsed_us);
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(ix, preview);
    }
}

impl Collected {
    /// Fold the graph's shape and the collected previews into the JSON
    /// the viewer reads.
    ///
    /// Node ids come from the *expanded* graph, so a function call's
    /// body appears as `call/body`. The call's own id names the body
    /// node the function outputs, so a call still has a picture of its
    /// result even with its internals folded away.
    pub fn into_payload(
        self,
        doc: &Document,
        graph: &Graph,
        tile: &str,
        params: &Map<String, Json>,
        sidecar: Option<&Path>,
    ) -> Result<Json, std::io::Error> {
        if let Some(dir) = sidecar {
            std::fs::create_dir_all(dir)?;
        }
        let mut previews = self.previews;

        let mut nodes = Vec::with_capacity(graph.len());
        for ix in 0..graph.len() {
            let id = graph.node_id(ix);
            let node = graph.node(ix);
            let (group, label) = match id.rsplit_once('/') {
                Some((g, l)) => (Some(g), l),
                None => (None, id),
            };

            let mut inputs = Vec::new();
            for (port_ix, spec) in node.inputs().iter().enumerate() {
                if let Some(src) = graph.incoming(ix, port_ix) {
                    inputs.push(json!({ "port": spec.name, "from": graph.node_id(src) }));
                }
            }
            // An asset name may carry a neighbour-tile suffix
            // (`basemap@1,0`); the pill is the source either way.
            let assets: Vec<String> = node
                .asset_inputs()
                .iter()
                .filter_map(|name| {
                    let base = name.split('@').next().unwrap_or(name);
                    doc.sources.contains_key(base).then(|| base.to_string())
                })
                .collect();

            let mut entry = json!({
                "id": id,
                "label": label,
                "group": group,
                "op": node.op_name(),
                "kind": kind_name(graph.output_kind(ix)),
                "inputs": inputs,
                "assets": assets,
                "output": ix == graph.output(),
            });
            let obj = entry.as_object_mut().expect("object literal");
            match previews.remove(&ix) {
                Some(p) => {
                    obj.insert("summary".into(), json!(p.summary));
                    obj.insert("ms".into(), json!(p.ms));
                    obj.insert("cached".into(), json!(p.cached));
                    obj.insert("empty".into(), json!(p.empty));
                    if let Some(d) = p.detail {
                        obj.insert("detail".into(), json!(d));
                    }
                    if let Some(png) = p.png {
                        let src = match sidecar {
                            Some(dir) => {
                                let name = format!("{}.png", id.replace('/', "__"));
                                std::fs::write(dir.join(&name), &png)?;
                                format!(
                                    "{}/{}",
                                    dir.file_name().unwrap_or_default().to_string_lossy(),
                                    name
                                )
                            }
                            None => format!(
                                "data:image/png;base64,{}",
                                base64::engine::general_purpose::STANDARD.encode(&png)
                            ),
                        };
                        obj.insert("img".into(), json!(src));
                    }
                }
                // Only reachable if a node sat outside the render — the
                // topological order covers everything the output needs.
                None => {
                    obj.insert("summary".into(), json!("not evaluated"));
                }
            }
            nodes.push(entry);
        }

        let sources: Vec<Json> = doc
            .sources
            .iter()
            .map(|(id, decl)| json!({ "id": id, "kind": source_kind(decl) }))
            .collect();

        Ok(json!({
            "style": doc.name,
            "tile": tile,
            "tile_size": self.canvas.tile_w,
            "pad": self.canvas.pad,
            "params": params,
            "sources": sources,
            "nodes": nodes,
            "output": graph.node_id(graph.output()),
        }))
    }
}

/// Splice a payload into the viewer page.
pub fn render_html(payload: &Json) -> String {
    // The payload is spliced into a `<script>` block, where `</` ends it
    // whatever it is nested inside. `\/` is a legal JSON escape for the
    // same character, so this stays parseable both ways.
    let json = payload.to_string().replace("</", "<\\/");
    VIEWER_HTML.replace("__EZU_GRAPH__", &json)
}

fn kind_name(kind: PortKind) -> &'static str {
    match kind {
        PortKind::Features => "features",
        PortKind::Raster => "raster",
        PortKind::Sprite => "sprite",
        PortKind::Brush => "brush",
        PortKind::Labels => "labels",
        PortKind::Scalar => "scalar",
        PortKind::ScalarField => "scalar-field",
    }
}

fn source_kind(decl: &SourceDecl) -> &'static str {
    match decl {
        SourceDecl::Brush(_) => "brush",
        SourceDecl::Image(_) => "image",
        SourceDecl::Mvt(_) => "mvt",
        SourceDecl::Pmtiles(_) => "pmtiles",
        SourceDecl::Dem(_) => "dem",
        SourceDecl::Raster(_) => "raster",
        SourceDecl::GeoJson(_) => "geojson",
        SourceDecl::Sprite(_) => "sprite",
        SourceDecl::Font(_) => "font",
        SourceDecl::Glyphs(_) => "glyphs",
    }
}

/// What a `features` port is actually carrying: how much geometry
/// survived the node's filter, and of what shape.
fn describe_features(f: &ezu::paint::nodes::FilteredFeatures) -> String {
    let (mut pt, mut ln, mut pg) = (0usize, 0usize, 0usize);
    for g in &f.groups {
        pt += g.points.len();
        ln += g.lines.len();
        pg += g.polygons.len();
    }
    let mut parts = Vec::new();
    if pt > 0 {
        parts.push(format!("{pt} pt"));
    }
    if ln > 0 {
        parts.push(format!("{ln} line"));
    }
    if pg > 0 {
        parts.push(format!("{pg} poly"));
    }
    if parts.is_empty() {
        return "nothing on this tile".to_string();
    }
    // A group is one source feature, except where a node synthesises
    // geometry (a contour, a scatter) and hands back a single group.
    let n = f.groups.len();
    format!(
        "{n} feature{} — {}, extent {}",
        if n == 1 { "" } else { "s" },
        parts.join(" · "),
        f.extent
    )
}

/// Paint a scalar field as greyscale, stretched over its own range so
/// a DEM and a noise field are both legible without a colour ramp.
/// Returns the picture and a description of the range it stretched.
fn scalar_field_to_gray(field: &ezu::graph::ScalarField) -> (RasterBuf, String) {
    let nodata = field.nodata;
    let is_data = |v: f32| v.is_finite() && nodata.is_none_or(|nd| v != nd);
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for &v in field.values.iter() {
        if is_data(v) {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    let mut buf = RasterBuf::new(field.width, field.height);
    if !lo.is_finite() || !hi.is_finite() {
        return (buf, "no data".to_string());
    }
    let span = if hi > lo { hi - lo } else { 1.0 };
    for (i, &v) in field.values.iter().enumerate() {
        let px = i * 4;
        if px + 3 >= buf.pixels.len() {
            break;
        }
        if !is_data(v) {
            continue; // transparent: no value here
        }
        let g = (((v - lo) / span).clamp(0.0, 1.0) * 255.0).round() as u8;
        buf.pixels[px..px + 4].copy_from_slice(&[g, g, g, 255]);
    }
    let range = match field.geo_scale {
        Some(gs) => format!("range {lo:.3} … {hi:.3}, {:.2} m/px", gs.metres_per_pixel_x),
        None => format!("range {lo:.3} … {hi:.3}"),
    };
    (buf, range)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ezu::graph::build_graph;
    use ezu::paint::nodes::default_registry;

    /// A call whose function body has one extra node, so the payload has
    /// both a call node and an internal one to distinguish.
    const STYLE: &str = r##"{
      "name": "t", "version": "1", "tile-size": 8,
      "functions": {
        "tint": {
          "inputs": { "base": { "kind": "raster" } },
          "output": "@mix", "output-kind": "raster",
          "nodes": {
            "over": { "op": "solid", "color": "#ff0000" },
            "mix": { "op": "blend", "base": "@base", "over": "@over", "mode": "multiply" }
          }
        }
      },
      "nodes": {
        "sheet": { "op": "solid", "color": "#ffffff" },
        "call": { "op": "func", "fn": "tint", "base": "@sheet" }
      },
      "output": "@call"
    }"##;

    fn payload() -> Json {
        let doc = Document::from_json(STYLE).unwrap();
        let graph = build_graph(&doc, &default_registry()).unwrap();
        let collected = Collected {
            canvas: CanvasInfo::square(8, 0),
            previews: HashMap::new(),
        };
        collected
            .into_payload(&doc, &graph, "0/0/0", &Map::new(), None)
            .unwrap()
    }

    fn node<'a>(p: &'a Json, id: &str) -> &'a Json {
        p["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == id)
            .unwrap_or_else(|| panic!("no node `{id}` in the payload"))
    }

    #[test]
    fn a_function_body_is_named_by_its_call() {
        let p = payload();
        // Expansion mangles a body node's id; the viewer folds it back
        // onto the call, which needs the two halves kept apart.
        let inner = node(&p, "call/over");
        assert_eq!(inner["group"], "call");
        assert_eq!(inner["label"], "over");
        assert_eq!(inner["op"], "solid");

        // The function's own output node keeps the call's id, so `@call`
        // still resolves — and the call gets a picture of its result.
        let call = node(&p, "call");
        assert_eq!(call["group"], Json::Null);
        assert_eq!(call["op"], "blend");
        assert_eq!(call["output"], true);
    }

    #[test]
    fn edges_follow_the_expanded_graph() {
        let p = payload();
        let ins = |id: &str| -> Vec<(String, String)> {
            node(&p, id)["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| {
                    (
                        i["port"].as_str().unwrap().to_string(),
                        i["from"].as_str().unwrap().to_string(),
                    )
                })
                .collect()
        };
        // The call's `base` argument became a plain edge from `sheet`,
        // and the body's own wiring survived as `call/over`.
        assert_eq!(
            ins("call"),
            vec![
                ("base".to_string(), "sheet".to_string()),
                ("over".to_string(), "call/over".to_string()),
            ]
        );
        assert!(ins("sheet").is_empty());
        assert_eq!(p["output"], "call");
    }

    #[test]
    fn a_node_that_never_evaluated_says_so() {
        let p = payload();
        assert_eq!(node(&p, "sheet")["summary"], "not evaluated");
        assert!(node(&p, "sheet").get("img").is_none());
    }
}
