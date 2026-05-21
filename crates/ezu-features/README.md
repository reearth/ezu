# ezu-features

GIS feature parsing for the [`ezu`](../../README.md) workspace.

Parses tile / feature formats into a flat, owned representation that
downstream paint code can iterate over without implementing any
format-specific traits. **Remote fetching is intentionally out of
scope** — input is always raw bytes / strings handed in by the caller
(e.g. an example, `ezu-server`, the WASM bindings).

## Submodules

| Module | Input | Output |
|---|---|---|
| [`mvt`](src/mvt.rs) | `&[u8]` MVT protobuf (gunzipped) | `DecodedTile` with one `DecodedLayer` per layer |
| [`geojson`](src/geojson.rs) | `&str` / `&[u8]` GeoJSON | `Vec<Feature>` (FeatureCollection or single Feature) |

All decoders produce the same crate-root types:

```rust
pub struct Feature {
    pub id: Option<u64>,
    pub geometry: Geometry,
    pub properties: HashMap<String, Value>,
}
pub enum Geometry { Points(...), Lines(...), Polygons(Vec<Polygon>), Unknown }
pub struct Polygon { pub exterior: Vec<(i32, i32)>, pub holes: Vec<Vec<(i32, i32)>> }
pub enum Value { String, Float, Double, Int, UInt, SInt, Bool, Null }
```

Coordinates are integer `(i32, i32)` pairs. For MVT this is the
spec-defined tile-local `[0, extent]` (y-down); for GeoJSON the caller
is responsible for any projection / quantization into the same
coordinate space before parsing.

## MVT example

```rust
let decoded = ezu_features::mvt::decode(mvt_bytes)?;
if let Some(water) = decoded.layer("water") {
    for f in &water.features {
        if let ezu_features::Geometry::Polygons(polys) = &f.geometry {
            // paint
        }
    }
}
```

Polygon rings are classified as exterior / hole via signed shoelace
area following the
[MVT spec](https://github.com/mapbox/vector-tile-spec).

## License

MIT or Apache-2.0, at your option.
