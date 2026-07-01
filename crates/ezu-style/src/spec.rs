//! The style spec data types: typed node-DAG documents parsed from
//! JSON. Re-exported at the crate root.

// JsonSchema generation is deferred — `schemars` 1.x has no `IndexMap`
// impl out of the box, and the schema will likely want hand-tuning
// (one entry per registered op) anyway. Derive serde only for now.

use std::collections::HashMap;

use indexmap::IndexMap;
use serde::Deserialize;

use crate::StyleError;

/// A parsed style document. Order of `nodes` is preserved (for
/// deterministic error messages) but does not imply evaluation order —
/// that is derived by topological sort of the DAG.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Document {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default = "default_tile_size")]
    pub tile_size: u32,
    #[serde(default)]
    pub pad: u32,
    #[serde(default)]
    pub params: IndexMap<String, ParamDecl>,
    /// Attribution for the style itself (HTML allowed, like MapLibre).
    /// Per-source attributions live on the `sources` entries; hosts
    /// merge both with upstream metadata (TileJSON / PMTiles) — see
    /// [`Document::attributions`].
    #[serde(default)]
    pub attribution: Option<String>,
    /// User-defined functions: reusable node subgraphs called with
    /// `{ "op": "func", "fn": "<name>", ...args }`. Expanded inline at
    /// graph-build time — see [`expand_functions`](crate::expand_functions).
    #[serde(default)]
    pub functions: IndexMap<String, FuncDecl>,
    /// External data the host provides. Mixes document-scoped resources
    /// (`brush`, `image`) — resolved once per style — and tile-scoped
    /// pyramids (`mvt`, `pmtiles`, `dem`) — fetched per tile. The
    /// `type` discriminator selects the variant.
    ///
    /// Per-tile variants bind their payload under `tile.<source-name>`
    /// for source nodes to consume. Document-scoped variants are
    /// referenced by `@source-name` in node fields (the legacy
    /// `assets` block from 0.2 is gone — its entries move here).
    #[serde(default)]
    pub sources: IndexMap<String, SourceDecl>,
    pub nodes: IndexMap<String, NodeSpec>,
    /// Node id (with or without `@` prefix) that produces the final raster.
    pub output: NodeRef,
}

impl Document {
    pub fn from_json(s: &str) -> Result<Self, StyleError> {
        Ok(serde_json::from_str(s)?)
    }

    /// Every attribution string declared in the document: the style's
    /// own `attribution` plus each source's, in declaration order,
    /// deduplicated. Upstream metadata (TileJSON `attribution`,
    /// PMTiles metadata) is a host concern — hosts merge it with this
    /// list after opening their sources.
    pub fn attributions(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        let candidates = std::iter::once(&self.attribution)
            .chain(self.sources.values().map(|d| d.attribution()));
        for a in candidates {
            if let Some(a) = a.as_deref() {
                if !a.is_empty() && !out.contains(&a) {
                    out.push(a);
                }
            }
        }
        out
    }

    /// JSON Schema describing the *parameter values* object a caller
    /// may pass when rendering this style (CLI `--param`, server query
    /// string, library `ParamValues`). Derived from the document's
    /// `params` declarations: numbers carry `minimum` / `maximum`,
    /// colors a hex-string pattern, and every entry its declared
    /// `default` / `description`. Editor UIs can drive sliders and
    /// color pickers straight off this.
    pub fn params_schema(&self) -> serde_json::Value {
        use serde_json::{json, Map, Value};
        let mut props = Map::new();
        for (name, decl) in &self.params {
            let mut p = match decl.kind {
                ParamKind::Number => {
                    let mut p = Map::new();
                    p.insert("type".into(), json!("number"));
                    if let Some(m) = decl.min {
                        p.insert("minimum".into(), json!(m));
                    }
                    if let Some(m) = decl.max {
                        p.insert("maximum".into(), json!(m));
                    }
                    p
                }
                ParamKind::Color => {
                    let mut p = Map::new();
                    p.insert("type".into(), json!("string"));
                    p.insert(
                        "pattern".into(),
                        json!("^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$"),
                    );
                    p.insert("format".into(), json!("color"));
                    p
                }
                ParamKind::Bool => {
                    let mut p = Map::new();
                    p.insert("type".into(), json!("boolean"));
                    p
                }
            };
            p.insert("default".into(), decl.default.clone());
            if let Some(d) = &decl.description {
                p.insert("description".into(), json!(d));
            }
            props.insert(name.clone(), Value::Object(p));
        }
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": format!("{} parameters", self.name),
            "type": "object",
            "additionalProperties": false,
            "properties": props,
        })
    }
}

fn default_version() -> String {
    "1".to_string()
}
fn default_tile_size() -> u32 {
    512
}

/// A user-defined function: a reusable node subgraph with declared
/// input ports and output kind. Shaped like a mini-document — `inputs`
/// play the role of `params`, `nodes` is the body, `output` names the
/// body node whose value the call produces.
///
/// Inside the body, `@<input-name>` references a function input;
/// `@<body-node>` references another body node; `@<source-name>`
/// reaches a document-scoped source. Anything else is an error —
/// functions are closed over their inputs (no implicit access to
/// caller nodes).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct FuncDecl {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub inputs: IndexMap<String, FuncInput>,
    /// Body node (with or without `@`) whose value the call produces.
    pub output: NodeRef,
    /// Declared kind of the output — verified against the body's
    /// resolved port kind at graph-build time.
    pub output_kind: FuncKind,
    pub nodes: IndexMap<String, NodeSpec>,
}

/// One declared function input.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct FuncInput {
    pub kind: FuncKind,
    /// Default argument — allowed for `scalar` inputs only; its
    /// presence makes the input optional at the call site. A `null`
    /// default (or argument) makes substituted fields disappear from
    /// the body node entirely — the way to feed optional op fields
    /// whose absence means something (e.g. stroke curves).
    #[serde(default, deserialize_with = "some_value")]
    pub default: Option<serde_json::Value>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Deserialize any present JSON value — *including* `null` — as
/// `Some(value)`, so `"default": null` is distinguishable from an
/// absent `default`.
fn some_value<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<serde_json::Value>, D::Error> {
    serde_json::Value::deserialize(d).map(Some)
}

/// Port-kind vocabulary for function signatures. Mirrors the graph's
/// `PortKind` names.
#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum FuncKind {
    Features,
    Raster,
    Sprite,
    Brush,
    Scalar,
    ScalarField,
}

impl FuncKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FuncKind::Features => "features",
            FuncKind::Raster => "raster",
            FuncKind::Sprite => "sprite",
            FuncKind::Brush => "brush",
            FuncKind::Scalar => "scalar",
            FuncKind::ScalarField => "scalar-field",
        }
    }
}

/// One node entry. `op` selects the implementation; remaining fields are
/// op-specific and are validated by the `NodeFactory` registered for `op`.
#[derive(Debug, Clone, Deserialize)]
pub struct NodeSpec {
    pub op: String,
    /// All remaining fields. Scalars are literals (color, number, bool);
    /// strings that begin with `@` are node references, strings that
    /// begin with `$` are param references.
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// Declaration of a document-level parameter (overridable at render time).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ParamDecl {
    #[serde(rename = "type")]
    pub kind: ParamKind,
    pub default: serde_json::Value,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum ParamKind {
    Color,
    Number,
    Bool,
}

/// What a tile-pyramid source does when a tile request 404s within
/// the source's zoom range. (Other HTTP failures are always errors;
/// requests past `max-zoom` always upsample from the ancestor at
/// `max-zoom`.)
#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OnMissing {
    /// Treat the tile as empty: transparent pixels for `raster`,
    /// zero elevation for `dem`. Missing *neighbour* tiles always
    /// degrade this way (the stitch edge-clamps).
    #[default]
    Empty,
    /// Walk up parent zooms until a tile exists and upsample the
    /// covered sub-region; falls back to `empty` when nothing is
    /// found all the way to z0.
    Upsample,
    /// Fail the whole tile render. Hosts surface it (the tile server
    /// returns HTTP 404 for the rendered tile).
    Error,
}

/// Declaration of one external data source. Mixes document-scoped
/// resources (`brush`, `image`) — resolved once per style from a file
/// path or `http(s)://` URL — and tile-scoped pyramids (`mvt`,
/// `pmtiles`, `dem`) — fetched per tile via a URL template.
///
/// The legacy `mask-image` / `gradient` kinds from 0.2 are gone:
/// `mask-image` was indistinguishable from `image` at runtime (host
/// decoded both as RGBA8), so callers compose `image` →
/// `pick-channel a` to get a single-channel mask; `gradient` was
/// never wired up and the `gradient-*` node family covers that use
/// case directly.
///
/// Binding conventions in the host's `TileLoader`:
/// - Document-scoped: looked up by the source name (referenced in
///   node fields as `@source-name`).
/// - `dem` binds a stitched `ScalarField` under `tile.<source-name>`.
/// - `mvt` and `pmtiles` bind every layer of the decoded vector tile
///   under `tile.<layer-name>` (i.e. by the layer's name *inside* the
///   tile, not the source key — built-in styles reference layers like
///   `tile.water` / `tile.roads`).
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SourceDecl {
    Brush(FileSource),
    Image(FileSource),
    Mvt(MvtSource),
    Pmtiles(PmtilesSource),
    Dem(DemSource),
    Raster(RasterSource),
    #[serde(rename = "geojson")]
    GeoJson(GeoJsonSource),
    Sprite(SpriteSource),
}

impl SourceDecl {
    /// The source's declared attribution, if any.
    pub fn attribution(&self) -> &Option<String> {
        match self {
            SourceDecl::Brush(s) | SourceDecl::Image(s) => &s.attribution,
            SourceDecl::Mvt(s) => &s.attribution,
            SourceDecl::Pmtiles(s) => &s.attribution,
            SourceDecl::Dem(s) => &s.attribution,
            SourceDecl::Raster(s) => &s.attribution,
            SourceDecl::GeoJson(s) => &s.attribution,
            SourceDecl::Sprite(s) => &s.attribution,
        }
    }
}

/// A sprite sheet: one atlas image plus a name → sub-rect index, so
/// `icon` nodes can crop named icons out of it (icons, `fill-pattern`).
/// The runtime shape is [`ezu_graph::SpriteSheet`]; the host builds it.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct SpriteSource {
    /// Atlas image `src` — a path (resolved against `--assets-dir`) or an
    /// `http(s)://` URL, like [`FileSource::src`]. This doubles as the
    /// sheet's asset key, so `icon { sprite: "@name" }` resolves here.
    pub image: String,
    /// The name → rect index: either a URL/path to a sprite `.json`, or an
    /// inline map. Inline uses the same field names as a fetched index.
    pub index: SpriteIndex,
    #[serde(default)]
    pub attribution: Option<String>,
}

/// A sprite index, inline or by reference.
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum SpriteIndex {
    /// URL/path to a sprite index JSON (fetched/read by the host).
    Url(String),
    /// Inline `name → rect` map written straight into the recipe.
    Inline(std::collections::HashMap<String, IconRect>),
}

/// One entry of a sprite index — a sub-rectangle of the atlas. Mirrors the
/// MapLibre sprite-JSON entry shape (extra keys like `sdf`/`content` are
/// ignored) so a fetched index deserializes into the same type.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct IconRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    #[serde(default = "one_f32")]
    pub pixel_ratio: f32,
}

fn one_f32() -> f32 {
    1.0
}

/// Inline (or URL) GeoJSON in WGS84 lon/lat. The host projects it into each
/// tile's local coordinate frame and binds it under `<source>.<source>`.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct GeoJsonSource {
    /// Inline GeoJSON (a `FeatureCollection`, `Feature`, or `Geometry`).
    #[serde(default)]
    pub data: Option<serde_json::Value>,
    /// URL to a `.geojson` document (fetched by the host) — alternative to
    /// inline `data`.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub attribution: Option<String>,
}

/// A document-scoped, file-based source. `src` is a path the host
/// resolves (relative to `--assets-dir`) or an `http(s)://` URL.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct FileSource {
    pub src: String,
    #[serde(default)]
    pub attribution: Option<String>,
}

/// Templated XYZ MVT tile source.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct MvtSource {
    /// XYZ URL template with `{z}`, `{x}`, `{y}` placeholders, or a
    /// TileJSON document URL (anything ending in `.json`).
    pub url: String,
    /// Explicit attribution. When absent, hosts inherit the upstream
    /// TileJSON `attribution` field.
    #[serde(default)]
    pub attribution: Option<String>,
}

/// PMTiles archive source — local path or `http(s)://` URL.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct PmtilesSource {
    pub url: String,
    /// Explicit attribution. When absent, hosts inherit the
    /// `attribution` key of the archive's metadata JSON.
    #[serde(default)]
    pub attribution: Option<String>,
}

/// Raster-DEM source. Tiles encode elevation in the RGB channels using
/// either the Mapzen / Terrarium scheme
/// (`h = (R*256 + G + B/256) - 32768`) or the Mapbox / MapLibre Terrain-RGB
/// scheme (`h = -10000 + (R*65536 + G*256 + B) * 0.1`).
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct DemSource {
    /// XYZ URL template with `{z}`, `{x}`, `{y}` placeholders, or a
    /// TileJSON document URL (anything ending in `.json`). PNG and
    /// WebP tiles are both supported (sniffed from content).
    pub url: String,
    pub encoding: DemEncoding,
    /// What a 404 within the zoom range means — empty (zero
    /// elevation), upsample from a parent, or fail the render.
    #[serde(default)]
    pub on_missing: OnMissing,
    /// Explicit attribution. When absent and `url` is a TileJSON,
    /// hosts inherit its `attribution` field.
    #[serde(default)]
    pub attribution: Option<String>,
    #[serde(default = "default_dem_tile_size")]
    pub tile_size: u32,
    /// Highest zoom available from the source. Requests above this zoom
    /// overzoom from an ancestor tile.
    #[serde(default)]
    pub max_zoom: Option<u8>,
    /// If true, fetch the 8 neighbouring tiles in addition to the
    /// centre tile and stitch them so gradient-based ops (e.g.
    /// `hillshade`) have seam-free samples in the pad region.
    #[serde(default = "default_true")]
    pub neighbor_fetch: bool,
    /// Value subtracted from each decoded sample (metres). Useful for
    /// rebasing geoid-relative datasets.
    #[serde(default)]
    pub elevation_offset: f32,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum DemEncoding {
    Terrarium,
    MapboxRgb,
}

/// RGBA raster tile pyramid (satellite imagery, pre-rendered
/// basemaps, …) consumed by the `raster` node as a canvas-sized
/// `Raster`. The host fetches the 3×3 neighbourhood per render and
/// stitches it onto the padded canvas, so downstream filters see
/// seamless pixels across tile borders.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct RasterSource {
    /// XYZ URL template with `{z}`, `{x}`, `{y}` placeholders, a
    /// TileJSON document URL (anything ending in `.json`), or a
    /// PMTiles archive (anything ending in `.pmtiles`; local path or
    /// `http(s)://` URL). PNG / WebP / JPEG tiles are sniffed from
    /// content.
    pub url: String,
    /// Highest zoom available from the source. Requests above this
    /// zoom upsample from the ancestor at `max-zoom`.
    #[serde(default)]
    pub max_zoom: Option<u8>,
    /// Fetch the 8 neighbouring tiles and stitch them so the pad
    /// region has real pixels.
    #[serde(default = "default_true")]
    pub neighbor_fetch: bool,
    /// What a 404 within the zoom range means — transparent pixels,
    /// upsample from a parent, or fail the render.
    #[serde(default)]
    pub on_missing: OnMissing,
    /// Explicit attribution. When absent, hosts inherit upstream
    /// metadata (TileJSON `attribution` / PMTiles metadata).
    #[serde(default)]
    pub attribution: Option<String>,
}

fn default_dem_tile_size() -> u32 {
    256
}
fn default_true() -> bool {
    true
}

/// A reference to a node id, optionally prefixed with `@`. The prefix is
/// stripped on parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRef(pub String);

impl NodeRef {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for NodeRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(NodeRef(s.strip_prefix('@').unwrap_or(&s).to_string()))
    }
}

/// Classify a string field on a node: a node reference, a param
/// reference, or a literal string. The classification is by prefix:
///
/// - `@name` → [`FieldRef::Node`]
/// - `$name` → [`FieldRef::Param`]
/// - anything else → [`FieldRef::Literal`]
pub enum FieldRef<'a> {
    Node(&'a str),
    Param(&'a str),
    Literal(&'a str),
}

impl<'a> FieldRef<'a> {
    pub fn classify(s: &'a str) -> Self {
        if let Some(rest) = s.strip_prefix('@') {
            FieldRef::Node(rest)
        } else if let Some(rest) = s.strip_prefix('$') {
            FieldRef::Param(rest)
        } else {
            FieldRef::Literal(s)
        }
    }
}

// ---------------------------------------------------------------------------
// Feature filter — shared property-matching DSL used by MVT-driven nodes.

/// Feature-property filter: every entry must match (AND).
///
/// ```json
/// "filter": {
///   "kind":        ["highway", "major_road"],   // value ∈ {…}
///   "is_bridge":   true,                         // exact match
///   "kind_detail": { "not": ["canal", "river"] } // negation
/// }
/// ```
pub type FeatureFilter = HashMap<String, FilterMatch>;

/// One filter clause: exact match, membership test, or negation.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum FilterMatch {
    One(FilterAtom),
    Any(Vec<FilterAtom>),
    Not(NotMatch),
}

/// `{ "not": <atom | [atoms]> }` — matches when the inner clause does not.
#[derive(Debug, Clone, Deserialize)]
pub struct NotMatch {
    pub not: NotInner,
}

/// Payload inside `not`: either a single atom or a list of atoms.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum NotInner {
    One(FilterAtom),
    Any(Vec<FilterAtom>),
}

/// Scalar literal used inside a filter clause.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum FilterAtom {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_document() {
        let json = r##"{
          "name": "demo",
          "nodes": {
            "src":  { "op": "image", "src": "assets/bg.png" },
            "blur": { "op": "blur", "input": "@src", "sigma": 3 }
          },
          "output": "@blur"
        }"##;
        let doc = Document::from_json(json).unwrap();
        assert_eq!(doc.name, "demo");
        assert_eq!(doc.nodes.len(), 2);
        assert_eq!(doc.output.as_str(), "blur");
        assert_eq!(doc.nodes["blur"].op, "blur");
        assert_eq!(doc.nodes["blur"].fields["input"], "@src");
    }

    #[test]
    fn parses_output_without_at_prefix() {
        let json = r##"{
          "name": "demo",
          "nodes": { "a": { "op": "image", "src": "x.png" } },
          "output": "a"
        }"##;
        let doc = Document::from_json(json).unwrap();
        assert_eq!(doc.output.as_str(), "a");
    }

    #[test]
    fn parses_params_and_sources() {
        let json = r##"{
          "name": "demo",
          "params": {
            "ink": { "type": "color", "default": "#000000" },
            "k":   { "type": "number", "default": 0.5, "min": 0, "max": 1 }
          },
          "sources": {
            "brush": { "type": "brush", "src": "assets/wet.myb" }
          },
          "nodes": { "out": { "op": "solid", "color": "$ink" } },
          "output": "@out"
        }"##;
        let doc = Document::from_json(json).unwrap();
        assert_eq!(doc.params["k"].kind, ParamKind::Number);
        assert!(matches!(doc.sources["brush"], SourceDecl::Brush(_)));
        assert_eq!(doc.params["k"].max, Some(1.0));
    }

    #[test]
    fn params_schema_reflects_declarations() {
        let json = r##"{
          "name": "demo",
          "params": {
            "ink": { "type": "color", "default": "#000000", "description": "Line color" },
            "k":   { "type": "number", "default": 0.5, "min": 0, "max": 1 },
            "on":  { "type": "bool", "default": true }
          },
          "nodes": { "out": { "op": "solid", "color": "$ink" } },
          "output": "@out"
        }"##;
        let doc = Document::from_json(json).unwrap();
        let schema = doc.params_schema();
        let props = &schema["properties"];
        assert_eq!(props["ink"]["type"], "string");
        assert_eq!(props["ink"]["default"], "#000000");
        assert_eq!(props["ink"]["description"], "Line color");
        assert_eq!(props["k"]["type"], "number");
        assert_eq!(props["k"]["minimum"], 0.0);
        assert_eq!(props["k"]["maximum"], 1.0);
        assert_eq!(props["on"]["type"], "boolean");
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn parses_raster_source_and_attributions() {
        let json = r##"{
          "name": "demo",
          "attribution": "Style © Demo",
          "sources": {
            "photo":   { "type": "raster",
                         "url": "https://example.com/{z}/{x}/{y}.jpg",
                         "max-zoom": 18, "on-missing": "upsample",
                         "attribution": "© Example Sat" },
            "archive": { "type": "raster", "url": "tiles.pmtiles" },
            "basemap": { "type": "mvt", "url": "https://example.com/t.json",
                         "attribution": "Style © Demo" }
          },
          "nodes": { "out": { "op": "raster", "source": "photo" } },
          "output": "@out"
        }"##;
        let doc = Document::from_json(json).unwrap();
        let SourceDecl::Raster(r) = &doc.sources["photo"] else {
            panic!("expected raster source");
        };
        assert_eq!(r.max_zoom, Some(18));
        assert_eq!(r.on_missing, OnMissing::Upsample);
        assert!(r.neighbor_fetch);
        let SourceDecl::Raster(r) = &doc.sources["archive"] else {
            panic!("expected raster source");
        };
        assert_eq!(r.on_missing, OnMissing::Empty);
        // Dedup: the doc attribution and basemap's identical one merge.
        assert_eq!(doc.attributions(), ["Style © Demo", "© Example Sat"]);
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let json = r##"{
          "name": "demo",
          "nodes": {},
          "output": "@x",
          "junk": 1
        }"##;
        assert!(Document::from_json(json).is_err());
    }

    #[test]
    fn parses_filter_variants() {
        let json = r##"{
          "kind":        "ocean",
          "kinds":       ["a", "b"],
          "kind_detail": { "not": "canal" },
          "kind_many":   { "not": ["x", "y"] }
        }"##;
        let f: FeatureFilter = serde_json::from_str(json).unwrap();
        assert!(matches!(f["kind"], FilterMatch::One(_)));
        assert!(matches!(f["kinds"], FilterMatch::Any(_)));
        let FilterMatch::Not(n) = &f["kind_detail"] else {
            panic!("expected Not")
        };
        assert!(matches!(n.not, NotInner::One(_)));
        let FilterMatch::Not(n) = &f["kind_many"] else {
            panic!("expected Not")
        };
        assert!(matches!(n.not, NotInner::Any(_)));
    }

    #[test]
    fn classify_field_refs() {
        assert!(matches!(FieldRef::classify("@foo"), FieldRef::Node("foo")));
        assert!(matches!(FieldRef::classify("$bar"), FieldRef::Param("bar")));
        assert!(matches!(
            FieldRef::classify("plain"),
            FieldRef::Literal("plain")
        ));
    }
}
