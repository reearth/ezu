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
    #[serde(default)]
    pub assets: IndexMap<String, AssetDecl>,
    /// Per-tile data sources resolved by the host before each render and
    /// bound as `tile.<source-name>` for source nodes to consume. The
    /// payload type is source-kind specific (DEM, etc.).
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
}

fn default_version() -> String {
    "1".to_string()
}
fn default_tile_size() -> u32 {
    512
}

/// One node entry. `op` selects the implementation; remaining fields are
/// op-specific and are validated by the `NodeFactory` registered for `op`.
#[derive(Debug, Deserialize)]
pub struct NodeSpec {
    pub op: String,
    /// All remaining fields. Scalars are literals (color, number, bool);
    /// strings that begin with `@` are node references, strings that
    /// begin with `$` are param references.
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// Declaration of a document-level parameter (overridable at render time).
#[derive(Debug, Deserialize)]
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

/// Declaration of a named asset (file-based source resolved by the host).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct AssetDecl {
    #[serde(rename = "type")]
    pub kind: AssetKind,
    pub src: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum AssetKind {
    Brush,
    Image,
    MaskImage,
    Gradient,
}

/// Declaration of a per-tile data source. The host fetches the
/// configured tiles before each render and binds the decoded payload
/// under `tile.<source-name>` for source nodes (e.g. `dem`) to consume.
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SourceDecl {
    Dem(DemSource),
}

/// Raster-DEM source. Tiles encode elevation in the RGB channels using
/// either the Mapzen / Terrarium scheme
/// (`h = (R*256 + G + B/256) - 32768`) or the Mapbox / MapLibre Terrain-RGB
/// scheme (`h = -10000 + (R*65536 + G*256 + B) * 0.1`).
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct DemSource {
    /// XYZ URL template with `{z}`, `{x}`, `{y}` placeholders. PNG and
    /// WebP are both supported (decided by content-type / extension).
    pub url: String,
    pub encoding: DemEncoding,
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
    fn parses_params_and_assets() {
        let json = r##"{
          "name": "demo",
          "params": {
            "ink": { "type": "color", "default": "#000000" },
            "k":   { "type": "number", "default": 0.5, "min": 0, "max": 1 }
          },
          "assets": {
            "brush": { "type": "brush", "src": "assets/wet.myb" }
          },
          "nodes": { "out": { "op": "solid", "color": "$ink" } },
          "output": "@out"
        }"##;
        let doc = Document::from_json(json).unwrap();
        assert_eq!(doc.params["k"].kind, ParamKind::Number);
        assert_eq!(doc.assets["brush"].kind, AssetKind::Brush);
        assert_eq!(doc.params["k"].max, Some(1.0));
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
