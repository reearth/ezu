//! GIS feature parsing for ezu.
//!
//! Parses tile / feature formats into a flat, owned representation that
//! downstream paint code can iterate over without implementing
//! format-specific traits. Remote fetching is intentionally *not* in
//! scope — input is always raw bytes / strings from the caller.
//!
//! Submodules:
//!
//! - [`mvt`] — Mapbox Vector Tile protobuf decode
//! - [`geojson`] — GeoJSON FeatureCollection parse
//!
//! All decoders produce the same shared [`Feature`] / [`Geometry`] /
//! [`Polygon`] / [`Value`] types defined at the crate root.

use std::collections::HashMap;

pub mod geojson;
pub mod mvt;

/// One decoded feature: geometry plus a properties bag.
#[derive(Debug)]
pub struct Feature {
    pub id: Option<u64>,
    pub geometry: Geometry,
    pub properties: HashMap<String, Value>,
}

/// Geometry in source-specific tile-local coordinates. For MVT this is
/// `[0, extent]` with y-down; for GeoJSON it's whatever the caller
/// chose to project into before parsing.
#[derive(Debug)]
pub enum Geometry {
    Points(Vec<(i32, i32)>),
    Lines(Vec<Vec<(i32, i32)>>),
    Polygons(Vec<Polygon>),
    Unknown,
}

/// A polygon with one exterior ring and zero or more interior holes.
#[derive(Debug)]
pub struct Polygon {
    pub exterior: Vec<(i32, i32)>,
    pub holes: Vec<Vec<(i32, i32)>>,
}

/// Untyped property value carried alongside a feature.
#[derive(Debug, Clone)]
pub enum Value {
    String(String),
    Float(f32),
    Double(f64),
    Int(i64),
    UInt(u64),
    SInt(i64),
    Bool(bool),
    Null,
}
