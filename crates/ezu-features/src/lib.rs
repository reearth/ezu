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
pub mod ops;

/// A single tile-local layer of decoded features. The producer (MVT
/// decoder, GeoJSON loader, host glue, …) normalizes coordinates into
/// `[0, extent]` with y-down before constructing this. Consumers in
/// `ezu-paint` read it via the unified `AssetLoader` under
/// `tile.<name>` bindings.
#[derive(Debug, Clone)]
pub struct FeatureLayer {
    pub name: String,
    /// Tile coordinate extent (typically 4096 for MVT). GeoJSON
    /// producers pick a convention and pre-project into it.
    pub extent: u32,
    pub features: Vec<Feature>,
}

/// One decoded feature: geometry plus a properties bag.
#[derive(Debug, Clone)]
pub struct Feature {
    pub id: Option<u64>,
    pub geometry: Geometry,
    pub properties: HashMap<String, Value>,
}

/// Geometry in source-specific tile-local coordinates. For MVT this is
/// `[0, extent]` with y-down; for GeoJSON it's whatever the caller
/// chose to project into before parsing.
///
/// All three layers (points / lines / polygons) co-exist on a single
/// feature. Single-vs-multi (`Point` vs `MultiPoint`, etc.) is
/// expressed by element count, and a GeoJSON `GeometryCollection` maps
/// onto whichever combination of the three vecs its children produce.
#[derive(Debug, Default, Clone)]
pub struct Geometry {
    pub points: Vec<(i32, i32)>,
    pub lines: Vec<Vec<(i32, i32)>>,
    pub polygons: Vec<Polygon>,
}

impl Geometry {
    pub fn is_empty(&self) -> bool {
        self.points.is_empty() && self.lines.is_empty() && self.polygons.is_empty()
    }

    /// Merge `other`'s vertices into `self`. Used when flattening a
    /// GeoJSON `GeometryCollection`.
    pub fn extend(&mut self, other: Geometry) {
        self.points.extend(other.points);
        self.lines.extend(other.lines);
        self.polygons.extend(other.polygons);
    }
}

/// A polygon with one exterior ring and zero or more interior holes.
#[derive(Debug, Clone)]
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
