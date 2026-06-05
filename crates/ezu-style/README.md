# ezu-style

**Ezu Style Spec** parser for the [`ezu`](../../README.md) workspace.

A pure `serde`-based reader for the node-DAG style language. Parsing
this crate produces a [`Document`] — a data structure, not an
evaluator. To execute a document, feed it to
[`ezu-graph::build_graph`](../ezu-graph) with a `NodeRegistry`.

## Spec at a glance

```json
{
  "name": "watercolor",
  "tile-size": 512,
  "pad": 24,
  "sources": {
    "glazing": { "type": "brush", "src": "builtin:watercolor_glazing" }
  },
  "nodes": {
    "bg":            { "op": "solid", "color": "#fbf6e6" },
    "water_feat":    { "op": "features", "layer": "water" },
    "water":         { "op": "fill-dabs", "features": "@water_feat",
                       "color": "#5876a0", "opacity": 0.22,
                       "radius-px": 7, "spacing-px": 3 },
    "out":           { "op": "blend", "base": "@bg", "over": "@water" }
  },
  "output": "@out"
}
```

References inside node fields use a prefix:

- `@name` — node reference (input wiring); on scalar fields this is
  a `Scalar` port, so computed values (`math`, `zoom`) plug in
- `$name` — `params` reference, resolved against caller-supplied
  values at render time (falling back to the declared default)

Each `features` node selects a layer with `source` (optional when the
document declares a single MVT-flavoured source) + `layer`, and
carries an optional `filter` (entries AND-combined; values are single
literals, membership lists, or `{ "not": ... }`), an optional
`min-zoom-field`, and optional `min-zoom` / `max-zoom` tile-zoom
gates.

The `sources` block is the single home for **every** external
resource the renderer pulls in — document-scoped files (brushes,
images) sit next to tile-scoped pyramids (MVT, PMTiles, DEM). Each
entry's `type` discriminates the variant; document-scoped variants
carry a `src` URI, tile-scoped variants a `url` template.

`src` URIs are explicit-scheme — pick one of:

- `builtin:NAME` — bundled brush / image included in `ezu-paint`'s
  built-in bank, or a host-registered resource of the same name.
- `file:PATH` — local file resolved against `--assets-dir` (or
  absolute). `.myb` / `.png` / `.webp` extensions are inferred from
  the source `type` if omitted.
- `http(s)://...` — fetched by the host (CLI native, `prefetch_doc_assets`)
  before the first render, then cached in the in-memory bank.

Tile-scoped source kinds:

- `dem` — raster-DEM tile pyramid (terrarium or mapbox-rgb encoding).
  The host stitches the 3×3 neighbourhood into a per-tile
  `ScalarField` (with `geo_scale` populated) and binds it under
  `tile.<source-name>`, ready for the `dem` source node to pick up.
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

## Params

The `params` block declares typed, documented knobs (`color` /
`number` / `bool`, with `default`, optional `min` / `max`, and
`description`). `$name` references resolve at render time against
caller-supplied values — the CLI's `--param`, the tile server's
query-string overrides, or a library `ParamValues` — so one built
graph serves every parameter combination. `Document::params_schema()`
derives a JSON Schema of the value object for editor UIs.

## Functions

Repeated node patterns factor into user-defined functions: a `functions`
block declares reusable subgraphs with typed input ports and an output
kind, and `op: "func"` calls them:

```jsonc
{
  "functions": {
    "sketchy-line": {
      "inputs": {
        "features": { "kind": "features" },
        "brush":    { "kind": "brush" },
        "color":    { "kind": "scalar" },
        "radius":   { "kind": "scalar", "default": 1.0 }
      },
      "output": "@draw",
      "output-kind": "raster",
      "nodes": {
        "wob":  { "op": "wave", "features": "@features", "amplitude-px": 0.8 },
        "draw": { "op": "line", "features": "@wob", "brush": "@brush",
                  "color": "@color", "radius-px": "@radius" }
      }
    }
  },
  "nodes": {
    "roads": { "op": "func", "fn": "sketchy-line",
               "features": "@roads_f", "brush": "@pencil", "color": "$ink" }
  }
}
```

Functions expand inline at graph-build time, like hygienic macros:

- Inside a body, `@name` resolves to a function input, another body
  node, or a document-scoped source — nothing else. Functions are
  closed over their inputs, so a typo can't silently capture a caller
  node.
- Arguments substitute structurally: literals stay literals, `$param`
  references keep their runtime-override behavior, `@node` arguments
  become port connections. Scalar arguments substitute verbatim — even
  into places plain `$param`s can't reach, like gradient stops or
  stroke-curve arrays. A `null` default (or argument) removes the
  substituted field entirely, for op fields whose absence is
  meaningful.
- Functions may call functions; cyclic calls are a build error reported
  with the cycle path (`a → b → a`).
- Expanded body nodes are namespaced `<call>/<node>` (the output node
  takes the call id), so build errors and `--verbose` logs read
  naturally. Because the intermediate cache is content-addressed, two
  calls with identical arguments share cache entries — inlining doesn't
  duplicate work.

[`pencil-sketch.json`](../ezu/examples/styles/pencil-sketch.json) is the live demo: its ten wave-then-line stroke
layers are one `sketchy-line` function, and the water hatching is a
`water-hatch` function that calls it.

## Types

```rust
pub struct Document {
    pub name: String,
    pub tile_size: u32,
    pub pad: u32,
    pub params: IndexMap<String, ParamDecl>,
    /// User-defined functions, expanded inline by `build_graph`.
    pub functions: IndexMap<String, FuncDecl>,
    /// Document- and tile-scoped external data: brushes, images,
    /// MVT / PMTiles / DEM pyramids.
    pub sources: IndexMap<String, SourceDecl>,
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
