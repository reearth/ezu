# ezu-features

GIS feature parsing for the [`ezu`](../../README.md) workspace.

Parses tile / feature formats into a flat, owned representation that
downstream paint code can iterate over without implementing any
format-specific traits. **Remote fetching is intentionally out of
scope** — input is always raw bytes / strings handed in by the caller
(e.g. an example, `ezu serve`, the WASM bindings).

## Submodules

| Module | Input | Output |
|---|---|---|
| [`mvt`](src/mvt.rs) | `&[u8]` MVT protobuf (gunzipped) | `DecodedTile` with one `FeatureLayer` per layer |
| [`geojson`](src/geojson.rs) | `&str` / `&[u8]` GeoJSON | `Vec<Feature>` (FeatureCollection or single Feature) |

All decoders produce the same crate-root types:

```rust
pub struct FeatureLayer {
    pub name: String,
    pub extent: u32,            // tile-local coord range, typically 4096 for MVT
    pub features: Vec<Feature>,
}
pub struct Feature {
    pub id: Option<u64>,
    pub geometry: Geometry,
    pub properties: HashMap<String, Value>,
}
pub struct Geometry {
    pub points: Vec<(i32, i32)>,
    pub lines: Vec<Vec<(i32, i32)>>,
    pub polygons: Vec<Polygon>,
}
pub struct Polygon { pub exterior: Vec<(i32, i32)>, pub holes: Vec<Vec<(i32, i32)>> }
pub enum Value { String, Float, Double, Int, UInt, SInt, Bool, Null }
```

Coordinates are integer `(i32, i32)` pairs. For MVT this is the
spec-defined tile-local `[0, extent]` (y-down); for GeoJSON the caller
is responsible for any projection / quantization into the same
coordinate space before parsing. All three geometry kinds coexist on a
single feature — single-vs-multi is expressed by element count, and a
GeoJSON `GeometryCollection` flattens into whichever combination its
children produce.

## MVT example

```rust
let decoded = ezu_features::mvt::decode(mvt_bytes)?;
if let Some(water) = decoded.layer("water") {
    for f in &water.features {
        for poly in &f.geometry.polygons {
            // paint
        }
    }
}
```

Polygon rings are classified as exterior / hole via signed shoelace
area following the
[MVT spec](https://github.com/mapbox/vector-tile-spec).

## Hand-off to the renderer

`ezu-paint` consumes feature data through the generic
`AssetLoader` interface, not by importing this crate directly. Hosts
decode their bytes into [`FeatureLayer`] and bind each layer under a
`tile.<layer-name>` name via
[`ezu_paint::host::TileLoader`](../ezu-paint) before rendering; the
style's `features` node samples the binding by name like a shader
uniform. This keeps the source-format choice (MVT today, GeoJSON or
anything else tomorrow) on the host side — no node code changes when
the wire format does.

## License

MIT or Apache-2.0, at your option.
