//! MVT (Mapbox Vector Tile) decoding for ezu.
//!
//! Decodes the protobuf to a flat, owned representation that downstream paint
//! code can iterate over without implementing `geozero` traits.

use std::collections::HashMap;

use geozero::mvt::{tile, Message, Tile};

#[derive(Debug, thiserror::Error)]
pub enum MvtError {
    #[error("mvt decode: {0}")]
    Decode(String),
}

/// Decode raw MVT bytes (already gunzipped) into owned layers.
pub fn decode(bytes: &[u8]) -> Result<DecodedTile, MvtError> {
    let tile = Tile::decode(bytes).map_err(|e| MvtError::Decode(e.to_string()))?;
    let layers = tile
        .layers
        .into_iter()
        .map(DecodedLayer::from_proto)
        .collect();
    Ok(DecodedTile { layers })
}

#[derive(Debug)]
pub struct DecodedTile {
    pub layers: Vec<DecodedLayer>,
}

impl DecodedTile {
    pub fn layer(&self, name: &str) -> Option<&DecodedLayer> {
        self.layers.iter().find(|l| l.name == name)
    }
}

#[derive(Debug)]
pub struct DecodedLayer {
    pub name: String,
    /// Tile coordinate extent (typically 4096).
    pub extent: u32,
    pub features: Vec<Feature>,
}

impl DecodedLayer {
    fn from_proto(layer: tile::Layer) -> Self {
        let extent = layer.extent.unwrap_or(4096);
        let values: Vec<Value> = layer.values.into_iter().map(Value::from_proto).collect();
        let keys = layer.keys;
        let features = layer
            .features
            .into_iter()
            .map(|f| Feature::from_proto(f, &keys, &values))
            .collect();
        Self {
            name: layer.name,
            extent,
            features,
        }
    }
}

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

impl Value {
    fn from_proto(v: tile::Value) -> Self {
        if let Some(s) = v.string_value {
            Self::String(s)
        } else if let Some(f) = v.float_value {
            Self::Float(f)
        } else if let Some(d) = v.double_value {
            Self::Double(d)
        } else if let Some(i) = v.int_value {
            Self::Int(i)
        } else if let Some(u) = v.uint_value {
            Self::UInt(u)
        } else if let Some(s) = v.sint_value {
            Self::SInt(s)
        } else if let Some(b) = v.bool_value {
            Self::Bool(b)
        } else {
            Self::Null
        }
    }
}

#[derive(Debug)]
pub struct Feature {
    pub id: Option<u64>,
    pub geometry: Geometry,
    pub properties: HashMap<String, Value>,
}

impl Feature {
    fn from_proto(f: tile::Feature, keys: &[String], values: &[Value]) -> Self {
        let mut properties = HashMap::with_capacity(f.tags.len() / 2);
        for chunk in f.tags.chunks_exact(2) {
            if let (Some(k), Some(v)) = (keys.get(chunk[0] as usize), values.get(chunk[1] as usize))
            {
                properties.insert(k.clone(), v.clone());
            }
        }
        let geom_type = f.r#type();
        let geometry = decode_geometry(&f.geometry, geom_type);
        Self {
            id: f.id,
            geometry,
            properties,
        }
    }
}

/// Geometry in MVT tile-local coordinates (`[0, extent]`, y-down).
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

fn decode_geometry(cmds: &[u32], geom_type: tile::GeomType) -> Geometry {
    let rings = walk_rings(cmds);
    match geom_type {
        tile::GeomType::Point => Geometry::Points(rings.into_iter().flatten().collect()),
        tile::GeomType::Linestring => Geometry::Lines(rings),
        tile::GeomType::Polygon => {
            let mut polys: Vec<Polygon> = Vec::new();
            for ring in rings {
                if is_exterior(&ring) {
                    polys.push(Polygon {
                        exterior: ring,
                        holes: Vec::new(),
                    });
                } else if let Some(last) = polys.last_mut() {
                    last.holes.push(ring);
                }
                // Holes appearing before any exterior are dropped (malformed).
            }
            Geometry::Polygons(polys)
        }
        _ => Geometry::Unknown,
    }
}

/// Walk MVT geometry commands into raw rings (without the implicit close vertex).
fn walk_rings(cmds: &[u32]) -> Vec<Vec<(i32, i32)>> {
    let mut rings: Vec<Vec<(i32, i32)>> = Vec::new();
    let mut current: Vec<(i32, i32)> = Vec::new();
    let mut cx: i32 = 0;
    let mut cy: i32 = 0;
    let mut i = 0;

    while i < cmds.len() {
        let header = cmds[i];
        i += 1;
        let id = header & 0x7;
        let count = (header >> 3) as usize;
        match id {
            1 => {
                // MoveTo
                if !current.is_empty() {
                    rings.push(std::mem::take(&mut current));
                }
                for _ in 0..count {
                    if i + 1 >= cmds.len() {
                        return rings;
                    }
                    cx = cx.wrapping_add(zigzag(cmds[i]));
                    cy = cy.wrapping_add(zigzag(cmds[i + 1]));
                    i += 2;
                    current.push((cx, cy));
                }
            }
            2 => {
                // LineTo
                for _ in 0..count {
                    if i + 1 >= cmds.len() {
                        return rings;
                    }
                    cx = cx.wrapping_add(zigzag(cmds[i]));
                    cy = cy.wrapping_add(zigzag(cmds[i + 1]));
                    i += 2;
                    current.push((cx, cy));
                }
            }
            7 => {
                // ClosePath: ring closes implicitly; no parameters.
            }
            _ => break,
        }
    }

    if !current.is_empty() {
        rings.push(current);
    }
    rings
}

#[inline]
fn zigzag(v: u32) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

/// MVT spec: exterior rings have positive signed area in tile-local (y-down) space.
fn is_exterior(ring: &[(i32, i32)]) -> bool {
    if ring.len() < 3 {
        return true;
    }
    let mut sum: i64 = 0;
    for i in 0..ring.len() {
        let (x1, y1) = ring[i];
        let (x2, y2) = ring[(i + 1) % ring.len()];
        sum += (x1 as i64) * (y2 as i64) - (x2 as i64) * (y1 as i64);
    }
    sum > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigzag_roundtrip() {
        assert_eq!(zigzag(0), 0);
        assert_eq!(zigzag(1), -1);
        assert_eq!(zigzag(2), 1);
        assert_eq!(zigzag(3), -2);
    }

    #[test]
    fn exterior_cw_in_y_down() {
        // (0,0) → (10,0) → (10,10) → (0,10): clockwise visually in y-down → exterior.
        let cw = vec![(0, 0), (10, 0), (10, 10), (0, 10)];
        assert!(is_exterior(&cw));
        let ccw = vec![(0, 0), (0, 10), (10, 10), (10, 0)];
        assert!(!is_exterior(&ccw));
    }
}
