//! Ezu Style Spec v0: declarative style specification for painterly map rendering.
//!
//! v0 is intentionally minimal — only the fields the watercolor reference
//! style needs are modelled. Unknown fields are rejected (`deny_unknown_fields`)
//! so typos surface immediately while the spec is in flux.
//!
//! ```no_run
//! let json = std::fs::read_to_string("watercolor.json").unwrap();
//! let style: ezu_style::Style = ezu_style::Style::from_json(&json).unwrap();
//! ```

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum StyleError {
    #[error("style parse: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("invalid color: {0}")]
    Color(String),
}

/// A parsed Ezu Style document.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Style {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default = "default_tile_size")]
    pub tile_size: u32,
    #[serde(default)]
    pub pad: u32,
    pub background: HexColor,
    pub layers: Vec<LayerSpec>,
}

impl Style {
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

/// One layer in the render pipeline. Discriminated by `type`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum LayerSpec {
    /// `tiny-skia` solid fill + optional outline + gaussian blur.
    FillSolid(FillSolidSpec),
    /// `hokusai` scatter-dab fill with world-deterministic jitter.
    FillDabs(FillDabsSpec),
    /// `hokusai` brush stroke along polylines.
    Line(LineSpec),
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct FillSolidSpec {
    pub id: String,
    pub source_layer: String,
    #[serde(default)]
    pub filter: Option<FeatureFilter>,
    #[serde(default)]
    pub min_zoom_field: Option<String>,
    pub fill: HexColor,
    #[serde(default = "one")]
    pub fill_alpha: f32,
    #[serde(default)]
    pub edge: Option<HexColor>,
    #[serde(default = "default_edge_width")]
    pub edge_width: f32,
    #[serde(default)]
    pub blur_sigma: f32,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct FillDabsSpec {
    pub id: String,
    pub source_layer: String,
    #[serde(default)]
    pub filter: Option<FeatureFilter>,
    #[serde(default)]
    pub min_zoom_field: Option<String>,
    /// Color in sRGB hex. Will be converted to linear sRGB for hokusai.
    pub color: HexColor,
    pub opacity: f32,
    pub radius_px: f32,
    #[serde(default = "default_hardness")]
    pub hardness: f32,
    #[serde(default = "one")]
    pub paint: f32,
    pub spacing_px: f32,
    #[serde(default = "default_position_jitter")]
    pub position_jitter: f32,
    #[serde(default)]
    pub size_jitter: f32,
    #[serde(default)]
    pub opacity_jitter: f32,
    #[serde(default)]
    pub value_jitter: f32,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LineSpec {
    pub id: String,
    pub source_layer: String,
    #[serde(default)]
    pub filter: Option<FeatureFilter>,
    #[serde(default)]
    pub min_zoom_field: Option<String>,
    /// Brush reference. `@name` = look up in the renderer's brush bank;
    /// otherwise treated as a path to a `.myb` file.
    pub brush: String,
    pub color: HexColor,
    /// Optional radius override in canvas pixels. Replaces the brush's
    /// `radius_logarithmic` base value before stroking.
    #[serde(default)]
    pub radius_px: Option<f32>,
    /// Optional opacity override in `[0, 1]`. Replaces the brush's
    /// `opaque` base value before stroking.
    #[serde(default)]
    pub opacity: Option<f32>,
    #[serde(default = "default_pressure_base")]
    pub pressure_base: f32,
    #[serde(default = "default_pressure_jitter")]
    pub pressure_jitter: f32,
    #[serde(default = "default_dtime")]
    pub dtime: f32,
}

fn one() -> f32 {
    1.0
}
fn default_edge_width() -> f32 {
    1.0
}
fn default_hardness() -> f32 {
    0.5
}
fn default_position_jitter() -> f32 {
    0.9
}
fn default_pressure_base() -> f32 {
    0.7
}
fn default_pressure_jitter() -> f32 {
    0.2
}
fn default_dtime() -> f32 {
    0.02
}

/// `#rrggbb` or `#rrggbbaa` color in sRGB.
#[derive(Debug, Clone, Copy)]
pub struct HexColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl HexColor {
    pub fn srgb_linear(self) -> [f32; 3] {
        [
            srgb_to_linear(self.r as f32 / 255.0),
            srgb_to_linear(self.g as f32 / 255.0),
            srgb_to_linear(self.b as f32 / 255.0),
        ]
    }
}

impl JsonSchema for HexColor {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "HexColor".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        serde_json::from_value(serde_json::json!({
            "type": "string",
            "title": "HexColor",
            "description": "sRGB color: `#rrggbb` or `#rrggbbaa`",
            "pattern": "^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$"
        }))
        .unwrap()
    }
}

impl<'de> Deserialize<'de> for HexColor {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        parse_hex(&s).ok_or_else(|| serde::de::Error::custom(format!("bad color: {s}")))
    }
}

fn parse_hex(s: &str) -> Option<HexColor> {
    let s = s.strip_prefix('#')?;
    let (r, g, b, a) = match s.len() {
        6 => (
            u8::from_str_radix(&s[0..2], 16).ok()?,
            u8::from_str_radix(&s[2..4], 16).ok()?,
            u8::from_str_radix(&s[4..6], 16).ok()?,
            255,
        ),
        8 => (
            u8::from_str_radix(&s[0..2], 16).ok()?,
            u8::from_str_radix(&s[2..4], 16).ok()?,
            u8::from_str_radix(&s[4..6], 16).ok()?,
            u8::from_str_radix(&s[6..8], 16).ok()?,
        ),
        _ => return None,
    };
    Some(HexColor { r, g, b, a })
}

/// Feature-property filter: every entry must match (AND).
///
/// ```json
/// "filter": {
///   "kind":     ["highway", "major_road"],   // value ∈ {…}
///   "is_bridge": true                          // exact match
/// }
/// ```
pub type FeatureFilter = HashMap<String, FilterMatch>;

/// A single filter clause: exact match or membership test.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum FilterMatch {
    One(FilterAtom),
    Any(Vec<FilterAtom>),
}

/// Scalar literal used inside a filter clause.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum FilterAtom {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_style() {
        let json = r##"{
          "name": "demo",
          "background": "#f8f5e8",
          "layers": []
        }"##;
        let s = Style::from_json(json).unwrap();
        assert_eq!(s.name, "demo");
        assert_eq!(s.tile_size, 512);
        assert_eq!(s.pad, 0);
    }

    #[test]
    fn parses_layer_types() {
        let json = r##"{
          "name": "demo",
          "background": "#ffffff",
          "layers": [
            { "type": "fill-solid", "id": "a", "source-layer": "earth", "fill": "#f0e8d0" },
            { "type": "fill-dabs",  "id": "b", "source-layer": "water",
              "color": "#5876a0", "opacity": 0.2, "radius-px": 6, "spacing-px": 3 },
            { "type": "line", "id": "c", "source-layer": "roads",
              "brush": "@watercolor_glazing", "color": "#3a2c20" }
          ]
        }"##;
        let s = Style::from_json(json).unwrap();
        assert_eq!(s.layers.len(), 3);
        assert!(matches!(s.layers[0], LayerSpec::FillSolid(_)));
        assert!(matches!(s.layers[1], LayerSpec::FillDabs(_)));
        assert!(matches!(s.layers[2], LayerSpec::Line(_)));
    }

    #[test]
    fn rejects_unknown_fields() {
        let json = r##"{
          "name": "demo",
          "background": "#ffffff",
          "layers": [],
          "what": 1
        }"##;
        assert!(Style::from_json(json).is_err());
    }
}
