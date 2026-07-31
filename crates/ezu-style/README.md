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
    "glazing": { "type": "brush", "src": "file:brushes/watercolor_glazing.myb" }
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
carries an optional `filter-expr` (a [MapLibre filter
expression](https://maplibre.org/maplibre-style-spec/expressions/),
e.g. `["all", ["==", ["get", "kind"], "water"], ["has", "name"]]`,
evaluated per feature via `maplibre-expr`), an optional
`min-zoom-field`, and optional `min-zoom` / `max-zoom` tile-zoom
gates.

The `sources` block is the single home for **every** external
resource the renderer pulls in — document-scoped files (brushes,
images) sit next to tile-scoped pyramids (MVT, PMTiles, DEM). Each
entry's `type` discriminates the variant; document-scoped variants
carry a `src` URI, tile-scoped variants a `url` template.

`src` URIs are explicit-scheme — pick one of:

- `builtin:NAME` — a resource the host registered in the in-memory
  bank at runtime under `NAME` (nothing is bundled into the library).
- `file:PATH` — local file resolved against `--assets-dir` (or
  absolute). `.myb` / `.png` / `.webp` extensions are inferred from
  the source `type` if omitted.
- `http(s)://...` — fetched by the host (CLI native, `prefetch_doc_assets`)
  before the first render, then cached in the in-memory bank.
- `data:[<mediatype>][;base64],<payload>` — a self-contained inline
  asset (e.g. a small `image` PNG or `.myb` brush), decoded in-process
  with no I/O, so it works in every host including wasm. `image/*` media
  types load as images; others are tried as a brush.
- `system:FAMILY` — `font` sources only: a face resolved by family name
  from the machine's installed fonts, which makes the recipe
  machine-dependent and is unavailable on wasm. See
  [the `system:` font scheme](../ezu-paint#the-system-font-scheme).

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
- `raster` — RGBA imagery pyramid (satellite photos, pre-rendered
  basemaps; PNG / WebP / JPEG). The `url` is an XYZ template, a
  TileJSON document, or a PMTiles archive (`.pmtiles`). The host
  stitches the 3×3 neighbourhood onto the padded canvas and binds it
  under the source name for the `raster` node, which emits a
  canvas-sized `Raster` ready for any downstream filter.
- `geojson` — inline (`data`) or remote (`url`) GeoJSON in WGS84
  lon/lat. The host projects it into each tile's local frame and
  binds it as one feature layer under `<source>.<source>`.

Document-scoped, besides `brush` / `image`:

- `sprite` — a sprite sheet: an atlas `image` (`file:` / `http(s)://`)
  plus an `index` mapping icon names to atlas sub-rects. The `index`
  is a URL/path to a sprite `.json`, or an inline map (same field
  shape). The host decodes the sheet up front; an `icon` node crops a
  named rect into a `Sprite` for `stamp` (symbol icons) or `tiling`
  (`fill-pattern`).
- `font` — outline font bytes (TTF / OTF / TTC) for the `text` node's
  `font` stack, named with a `url`: a font file (`file:` / `http(s)://`
  / `data:`) or a `system:` family reference.
- `glyphs` — a MapLibre glyph-PBF endpoint (`{fontstack}` / `{range}`
  URL template) serving pre-rendered SDF glyph ranges, fetched lazily.
  An alternative to `font` that needs no font files. See
  [text labels](../ezu-paint#text-labels).

Tile pyramids (`raster`, `dem`) take an `on-missing` policy deciding
what an in-range 404 means: `empty` (default — transparent pixels /
zero elevation), `upsample` (walk up parent zooms and upsample the
covered sub-region), or `error` (fail the tile render; the tile
server returns HTTP 404). Requests past `max-zoom` always upsample
from the `max-zoom` ancestor.

Every source (and the document itself, via a top-level `attribution`
field) may declare an `attribution` string. Sources that don't
declare one inherit upstream metadata — the TileJSON `attribution`
field or the PMTiles archive metadata — when the host opens them.
`Document::attributions()` returns the declared list; the tile
server's `GET /style/attribution` serves the fully merged result.

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
`description`), referenced with `$name` anywhere a scalar field lives:

```jsonc
{
  "params": {
    "paper":    { "type": "color",  "default": "#fbf6e6" },
    "softness": { "type": "number", "default": 0, "min": 0, "max": 4,
                  "description": "Blur over the finished tile, in px." }
  },
  "nodes": {
    "bg":  { "op": "solid", "color": "$paper" },
    "out": { "op": "blur", "input": "@c4", "sigma": "$softness" }
  }
}
```

References resolve at **render time** against caller-supplied values —
the CLI's `--param`, the tile server's query-string overrides, or a
library `ParamValues` — so one built graph serves every parameter
combination, and the intermediate cache keys on the values a node
actually reads, so flipping one param only re-evaluates the nodes that
depend on it.

```sh
# CLI: repeatable --param flags, validated against the declarations.
ezu tile --style watercolor.json --tile 13/7276/3225 \
  --param 'paper=#ffe0f0' --param softness=2 --out tile.png

# Tile server: query-string overrides on the tile endpoint.
curl 'http://127.0.0.1:8080/tiles/13/7276/3225.png?paper=%23ffe0f0&softness=2'

# JSON Schema for the current style's parameters (defaults, ranges,
# descriptions) — drive sliders / color pickers off this.
curl http://127.0.0.1:8080/style/params
```

`Document::params_schema()` derives that schema for editor UIs; `ezu
serve`'s params panel is generated straight from it.

For computed values, wire scalars through the graph: `math` does
arithmetic over numbers (literals, `$param`s, or `@node` scalar ports)
and `zoom` emits the tile's zoom level, so zoom-dependent styling is a
two-node chain:

```jsonc
"z":        { "op": "zoom" },
"zfrac":    { "op": "math", "fn": "div", "a": "@z", "b": 16 },
"lu_alpha": { "op": "math", "fn": "mul", "a": "$landuse-alpha", "b": "@zfrac" },
"landuse":  { "op": "fill-solid", "features": "@landuse_feat",
              "fill": "#a6c084", "fill-alpha": "@lu_alpha" }
```

For MapLibre-native curves, `expr` evaluates a full MapLibre expression
once per tile (with the tile's zoom in the context) and emits a `Scalar`
you can feed to any node's scalar field.

One constraint: fields that decide canvas padding at build time (blur
sigmas and friends) need a static upper bound — a literal, or a
`$param` with `max` declared. Wiring those from a `@node` port is a
build error.

[`watercolor.json`](../ezu/examples/styles/watercolor.json) is a
complete parametric style.

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
```

## Example

```rust
let doc = ezu_style::Document::from_json(&json_text)?;
println!("{} ({} nodes)", doc.name, doc.nodes.len());
```

## License

MIT or Apache-2.0, at your option.
