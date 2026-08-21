//! MVT (Mapbox Vector Tile) decoding.
//!
//! Decodes the protobuf to a flat, owned representation built on the
//! crate-root [`Feature`] / [`Geometry`] / [`Polygon`] / [`Value`]
//! types.

use std::collections::HashMap;

use ezu_core::TileId;
use geozero::mvt::{tile, Message, Tile};

use crate::{Feature, FeatureLayer, Geometry, Polygon, Value};

#[derive(Debug, thiserror::Error)]
pub enum MvtError {
    #[error("mvt decode: {0}")]
    Decode(String),
    #[error("clip target {target:?} is not a descendant of {parent:?}")]
    NotDescendant { parent: TileId, target: TileId },
}

/// Decode raw MVT bytes (already gunzipped) into owned layers.
pub fn decode(bytes: &[u8]) -> Result<DecodedTile, MvtError> {
    let tile = Tile::decode(bytes).map_err(|e| MvtError::Decode(e.to_string()))?;
    let layers = tile.layers.into_iter().map(decode_layer).collect();
    Ok(DecodedTile { layers })
}

#[derive(Debug)]
pub struct DecodedTile {
    pub layers: Vec<FeatureLayer>,
}

impl DecodedTile {
    pub fn layer(&self, name: &str) -> Option<&FeatureLayer> {
        self.layers.iter().find(|l| l.name == name)
    }
}

fn decode_layer(layer: tile::Layer) -> FeatureLayer {
    let extent = layer.extent.unwrap_or(4096);
    let values: Vec<Value> = layer.values.into_iter().map(value_from_proto).collect();
    let keys = layer.keys;
    let features = layer
        .features
        .into_iter()
        .map(|f| feature_from_proto(f, &keys, &values))
        .collect();
    FeatureLayer {
        name: layer.name,
        extent,
        features,
    }
}

fn value_from_proto(v: tile::Value) -> Value {
    if let Some(s) = v.string_value {
        Value::String(s)
    } else if let Some(f) = v.float_value {
        Value::Float(f)
    } else if let Some(d) = v.double_value {
        Value::Double(d)
    } else if let Some(i) = v.int_value {
        Value::Int(i)
    } else if let Some(u) = v.uint_value {
        Value::UInt(u)
    } else if let Some(s) = v.sint_value {
        Value::SInt(s)
    } else if let Some(b) = v.bool_value {
        Value::Bool(b)
    } else {
        Value::Null
    }
}

fn feature_from_proto(f: tile::Feature, keys: &[String], values: &[Value]) -> Feature {
    let mut properties = HashMap::with_capacity(f.tags.len() / 2);
    let (tags, _) = f.tags.as_chunks::<2>();
    for &[key, value] in tags {
        if let (Some(k), Some(v)) = (keys.get(key as usize), values.get(value as usize)) {
            properties.insert(k.clone(), v.clone());
        }
    }
    let geom_type = f.r#type();
    let geometry = decode_geometry(&f.geometry, geom_type);
    Feature {
        id: f.id,
        geometry,
        properties,
    }
}

fn decode_geometry(cmds: &[u32], geom_type: tile::GeomType) -> Geometry {
    let rings = walk_rings(cmds);
    let mut g = Geometry::default();
    match geom_type {
        tile::GeomType::Point => g.points = rings.into_iter().flatten().collect(),
        tile::GeomType::Linestring => g.lines = rings,
        tile::GeomType::Polygon => {
            for ring in rings {
                if is_exterior(&ring) {
                    g.polygons.push(Polygon {
                        exterior: ring,
                        holes: Vec::new(),
                    });
                } else if let Some(last) = g.polygons.last_mut() {
                    last.holes.push(ring);
                }
                // Holes appearing before any exterior are dropped (malformed).
            }
        }
        _ => {}
    }
    g
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

/// Overzoom helper: re-express a decoded parent tile as if it had been
/// natively encoded at `descendant`'s zoom level. Each feature's
/// vertices are translated and scaled into the descendant's own
/// `[0, extent]` frame; features whose bounding box doesn't intersect
/// the descendant's region are dropped.
///
/// Used by hosts to fall back on a higher-zoom (parent) tile when the
/// requested tile is missing (e.g. PMTiles archive ends at zoom 12 but
/// the renderer asks for zoom 14). The library performs only the
/// coordinate transform — fetching / 404 detection is the host's job.
///
/// Geometry that straddles the descendant's edges is *not* clipped;
/// vertices outside `[0, extent]` after the transform are passed
/// through, matching MVT's "buffer" convention. Downstream rasterizers
/// already cope with out-of-tile vertices.
pub fn clip_to_descendant(
    parent_decoded: &DecodedTile,
    parent_id: TileId,
    descendant_id: TileId,
) -> Result<DecodedTile, MvtError> {
    if !parent_id.is_ancestor_of(descendant_id) {
        return Err(MvtError::NotDescendant {
            parent: parent_id,
            target: descendant_id,
        });
    }
    let dz = descendant_id.z - parent_id.z;
    let scale = 1u32 << dz;
    // Descendant's offset within the parent in tile units.
    let sub_x = descendant_id.x - parent_id.x * scale;
    let sub_y = descendant_id.y - parent_id.y * scale;

    let layers = parent_decoded
        .layers
        .iter()
        .map(|layer| clip_layer(layer, sub_x, sub_y, scale))
        .collect();
    Ok(DecodedTile { layers })
}

fn clip_layer(layer: &FeatureLayer, sub_x: u32, sub_y: u32, scale: u32) -> FeatureLayer {
    let extent = layer.extent as i64;
    // Edge length of the descendant's sub-region in parent coords.
    let sub_extent = extent / scale as i64;
    let ox = sub_x as i64 * sub_extent;
    let oy = sub_y as i64 * sub_extent;
    let scale_i = scale as i64;

    // Drop features whose bbox lies entirely outside the descendant
    // window. Vertices on or just past the edge survive — the
    // downstream renderer handles out-of-tile clipping.
    let features: Vec<Feature> = layer
        .features
        .iter()
        .filter_map(|f| {
            let bbox = geometry_bbox(&f.geometry)?;
            if bbox.max_x < ox || bbox.min_x > ox + sub_extent {
                return None;
            }
            if bbox.max_y < oy || bbox.min_y > oy + sub_extent {
                return None;
            }
            Some(Feature {
                id: f.id,
                geometry: transform_geometry(&f.geometry, ox, oy, scale_i),
                properties: f.properties.clone(),
            })
        })
        .collect();

    FeatureLayer {
        name: layer.name.clone(),
        extent: layer.extent,
        features,
    }
}

struct Bbox {
    min_x: i64,
    min_y: i64,
    max_x: i64,
    max_y: i64,
}

fn geometry_bbox(g: &Geometry) -> Option<Bbox> {
    let mut it = std::iter::empty()
        .chain(g.points.iter().copied())
        .chain(g.lines.iter().flatten().copied())
        .chain(
            g.polygons
                .iter()
                .flat_map(|p| p.exterior.iter().chain(p.holes.iter().flatten()).copied()),
        );
    let (x0, y0) = it.next()?;
    let mut bb = Bbox {
        min_x: x0 as i64,
        min_y: y0 as i64,
        max_x: x0 as i64,
        max_y: y0 as i64,
    };
    for (x, y) in it {
        let x = x as i64;
        let y = y as i64;
        bb.min_x = bb.min_x.min(x);
        bb.min_y = bb.min_y.min(y);
        bb.max_x = bb.max_x.max(x);
        bb.max_y = bb.max_y.max(y);
    }
    Some(bb)
}

fn transform_geometry(g: &Geometry, ox: i64, oy: i64, scale: i64) -> Geometry {
    let xf = |(x, y): (i32, i32)| -> (i32, i32) {
        let nx = (x as i64 - ox) * scale;
        let ny = (y as i64 - oy) * scale;
        (
            nx.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            ny.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        )
    };
    Geometry {
        points: g.points.iter().copied().map(xf).collect(),
        lines: g
            .lines
            .iter()
            .map(|ring| ring.iter().copied().map(xf).collect())
            .collect(),
        polygons: g
            .polygons
            .iter()
            .map(|p| Polygon {
                exterior: p.exterior.iter().copied().map(xf).collect(),
                holes: p
                    .holes
                    .iter()
                    .map(|h| h.iter().copied().map(xf).collect())
                    .collect(),
            })
            .collect(),
    }
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

    fn point_feature(x: i32, y: i32) -> Feature {
        Feature {
            id: None,
            geometry: Geometry {
                points: vec![(x, y)],
                ..Default::default()
            },
            properties: HashMap::new(),
        }
    }

    fn tile_with_points(extent: u32, pts: &[(i32, i32)]) -> DecodedTile {
        DecodedTile {
            layers: vec![FeatureLayer {
                name: "pts".into(),
                extent,
                features: pts.iter().map(|&(x, y)| point_feature(x, y)).collect(),
            }],
        }
    }

    #[test]
    fn clip_top_left_quadrant() {
        // Parent extent 4096. Descendant = top-left quadrant
        // (z+1, 2x, 2y) → sub-region [0, 2048) × [0, 2048) in parent
        // coords, scaled ×2 into descendant's own extent.
        let parent = TileId::new(10, 100, 200);
        let descendant = TileId::new(11, 200, 400);
        let src = tile_with_points(
            4096,
            &[
                (100, 100),   // inside top-left sub-region
                (2000, 1000), // inside
                (3000, 1000), // outside in x
                (1000, 3000), // outside in y
            ],
        );
        let out = clip_to_descendant(&src, parent, descendant).unwrap();
        let pts = &out.layers[0].features;
        assert_eq!(pts.len(), 2);
        // (100, 100) → (200, 200) after ×2 scale.
        assert_eq!(pts[0].geometry.points, vec![(200, 200)]);
        // (2000, 1000) → (4000, 2000).
        assert_eq!(pts[1].geometry.points, vec![(4000, 2000)]);
    }

    #[test]
    fn clip_bottom_right_quadrant() {
        let parent = TileId::new(5, 10, 20);
        // Bottom-right child: (z+1, 2x+1, 2y+1) → sub_x=1, sub_y=1.
        let descendant = TileId::new(6, 21, 41);
        let src = tile_with_points(4096, &[(2049, 2049), (2048, 2048), (0, 0)]);
        let out = clip_to_descendant(&src, parent, descendant).unwrap();
        let pts: Vec<_> = out.layers[0]
            .features
            .iter()
            .flat_map(|f| f.geometry.points.iter().copied())
            .collect();
        // (2049, 2049) → ((2049-2048)*2, (2049-2048)*2) = (2, 2).
        // (2048, 2048) → (0, 0). (0, 0) is outside; dropped.
        assert!(pts.contains(&(2, 2)));
        assert!(pts.contains(&(0, 0)));
        assert_eq!(pts.len(), 2);
    }

    #[test]
    fn clip_two_zooms_deep() {
        // dz=2 → scale ×4. Parent (8,1,1) covers a 4096-extent tile;
        // descendant (10, 5, 6) is at sub_x=1, sub_y=2 of the 4×4 grid.
        let parent = TileId::new(8, 1, 1);
        let descendant = TileId::new(10, 5, 6);
        let src = tile_with_points(4096, &[(1100, 2100)]);
        // Sub-extent = 4096/4 = 1024. sub_x=1 → ox=1024, sub_y=2 → oy=2048.
        // (1100, 2100) → ((1100-1024)*4, (2100-2048)*4) = (304, 208).
        let out = clip_to_descendant(&src, parent, descendant).unwrap();
        assert_eq!(out.layers[0].features[0].geometry.points, vec![(304, 208)]);
    }

    #[test]
    fn clip_rejects_non_descendant() {
        let parent = TileId::new(5, 10, 20);
        // Same zoom is not a descendant.
        assert!(matches!(
            clip_to_descendant(&tile_with_points(4096, &[(0, 0)]), parent, parent),
            Err(MvtError::NotDescendant { .. })
        ));
        // Different branch of the tree.
        let unrelated = TileId::new(6, 0, 0);
        assert!(matches!(
            clip_to_descendant(&tile_with_points(4096, &[(0, 0)]), parent, unrelated),
            Err(MvtError::NotDescendant { .. })
        ));
    }
}
