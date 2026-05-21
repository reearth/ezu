# ezu-mvt

MVT (Mapbox Vector Tile) decoder for the [`ezu`](../../README.md) workspace.

Wraps the `geozero` MVT protobuf reader and walks each layer into a flat,
owned representation that downstream paint code can iterate over without
implementing any `geozero` traits.

## API

```rust
pub fn decode(bytes: &[u8]) -> Result<DecodedTile, MvtError>;

pub struct DecodedTile { pub layers: Vec<DecodedLayer> }
pub struct DecodedLayer {
    pub name: String,
    pub extent: u32,
    pub features: Vec<Feature>,
}
pub struct Feature {
    pub id: Option<u64>,
    pub geometry: Geometry,
    pub properties: HashMap<String, Value>,
}
pub enum Geometry { Points, Lines, Polygons(Vec<Polygon>), Unknown }
pub struct Polygon { pub exterior: Vec<(i32, i32)>, pub holes: Vec<Vec<(i32, i32)>> }
```

Coordinates are MVT tile-local `[0, extent]` (y-down). Polygon rings are
classified as exterior / hole via signed shoelace area following the
[MVT spec](https://github.com/mapbox/vector-tile-spec).

## Example

```rust
let decoded = ezu_mvt::decode(mvt_bytes)?;
if let Some(water) = decoded.layer("water") {
    for f in &water.features {
        match &f.geometry {
            ezu_mvt::Geometry::Polygons(polys) => { /* paint */ }
            _ => {}
        }
    }
}
```

See the main [README](../../README.md) for the full project overview.

## License

MIT or Apache-2.0, at your option.
