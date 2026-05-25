# ezu-style

**Ezu Style Spec** parser for the [`ezu`](../../README.md) workspace.

A pure `serde`-based reader for the node-DAG style language. Parsing
this crate produces a [`Document`] — a data structure, not an
evaluator. To execute a document, feed it to
[`ezu-graph::build_graph`](../ezu-graph) with a `NodeRegistry`.

## Spec at a glance

```json
{
  "name": "watercolor-basic",
  "tile-size": 512,
  "pad": 24,
  "assets": {
    "glazing": { "type": "brush", "src": "watercolor_glazing" }
  },
  "nodes": {
    "bg":            { "op": "solid", "color": "#fbf6e6" },
    "water_feat":    { "op": "features", "name": "tile.water" },
    "water":         { "op": "fill-dabs", "features": "@water_feat",
                       "color": "#5876a0", "opacity": 0.22,
                       "radius-px": 7, "spacing-px": 3 },
    "out":           { "op": "blend", "base": "@bg", "over": "@water" }
  },
  "output": "@out"
}
```

References inside node fields use a prefix:

- `@name` — node reference (input wiring)
- `$name` — `params` substitution at build time

Each `features` node references a host-bound layer by `name`
(`tile.<layer>` for per-tile MVT/GeoJSON data) and carries an optional
`filter` (entries AND-combined; values are single literals or membership
lists) and an optional `min-zoom-field`.

Asset `src` strings (in the `assets` block) accept either a local file
reference resolved against the host's base directory, or an
`http(s)://` URL — native hosts (CLI, server, the tokyo example)
prefetch URL assets via `ezu_paint::host::prefetch_doc_assets` before
the first render, so the loader sees an already-decoded bank.

The `sources` block declares per-tile data sources the host fetches
+ binds before each render. Three source kinds are recognised today:

- `dem` — raster-DEM tile pyramid (terrarium or mapbox-rgb encoding).
  The host stitches the 3×3 neighbourhood into a per-tile
  `ScalarField` (with `geo_scale` populated) and binds it under `tile.<source-name>`, ready for
  the `dem` source node to pick up.
- `mvt` — XYZ MVT URL template (or a TileJSON document). The host
  fetches one tile per render, decodes every layer, and binds each
  one under `tile.<layer-name>` — the same names existing
  `features` nodes already reference.
- `pmtiles` — PMTiles archive (local path or `http(s)://` URL).
  Decoded layers bind the same way as `mvt`.

```json
"sources": {
  "terrain":  { "type": "dem", "encoding": "terrarium",
                 "url": "https://terrain.reearth.land/mapterhorn-egm08/terrarium/ellipsoid/{z}/{x}/{y}.webp",
                 "tile-size": 512, "max-zoom": 14 },
  "basemap":  { "type": "mvt",
                 "url": "https://example.com/tiles/{z}/{x}/{y}.pbf" }
}
```

For `mvt` / `pmtiles`, the source key (`basemap` above) is a label —
bindings still use the layer names from the decoded tile. Declare only
one MVT-flavoured source per style; later entries are ignored.

## Types

```rust
pub struct Document {
    pub name: String,
    pub tile_size: u32,
    pub pad: u32,
    pub params: IndexMap<String, ParamDecl>,
    pub assets: IndexMap<String, AssetDecl>,
    pub sources: IndexMap<String, SourceDecl>,   // per-tile data (DEM, …)
    pub nodes: IndexMap<String, NodeSpec>,
    pub output: NodeRef,
}
pub struct NodeSpec { pub op: String, pub fields: serde_json::Map<String, Value> }
pub type FeatureFilter = HashMap<String, FilterMatch>;
pub enum FilterMatch { One(FilterAtom), Any(Vec<FilterAtom>) }
pub enum FilterAtom { Bool, Int, Float, Str }
```

## Example

```rust
let doc = ezu_style::Document::from_json(&json_text)?;
println!("{} ({} nodes)", doc.name, doc.nodes.len());
```

## License

MIT or Apache-2.0, at your option.
