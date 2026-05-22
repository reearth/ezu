# ezu-graph

Typed DAG evaluator for the [`ezu`](../../README.md) painterly map
renderer.

[`ezu-style`](../ezu-style) parses a style JSON into a [`Document`]
(pure data). `ezu-graph` turns that document into an evaluatable graph
using a registry of node factories, then renders one tile per call.

The crate has no rendering deps of its own — `tiny-skia` / `hokusai` /
`libblur` all live in [`ezu-paint`](../ezu-paint), which registers the
concrete node implementations.

## Port types

The DAG is typed. Every edge carries one of four `PortKind`s:

| Kind | Carries |
|---|---|
| `Features` | Vector features (geometry + properties), pre-filtered. Source-format agnostic — MVT, GeoJSON, or synthesized in-node. |
| `Raster` | RGBA8 buffer (sRGB premultiplied), padded canvas-sized |
| `Brush` | hokusai brush handle plus overrides |
| `Scalar` | constant Color / Number / Bool |

`Features` and `Brush` ride as type-erased `Arc<dyn Any + Send + Sync>`
so producer / consumer nodes can use whatever concrete payload they
agree on (e.g. `ezu-paint::nodes::FilteredFeatures` for `Features`).

## Lifecycle

```rust
let doc      = ezu_style::Document::from_json(&json)?;
let registry = ezu_paint::nodes::default_registry();
let graph    = ezu_graph::build_graph(&doc, &registry)?;   // built once per style
let cache    = ezu_graph::Cache::new();                    // shared across tiles
let base     = ezu_paint::host::BrushBankLoader::default();

// Per-tile loader: tile-scoped feature layers overlaid on the base
// loader. Source nodes (`features`) sample bindings by name through
// the same `AssetLoader` trait — like reading shader uniforms.
let tile_id = ezu_graph::TileId { z: 13, x: 7276, y: 3225 };
let mut tile_loader = ezu_paint::host::TileLoader::new(&base, tile_id);
tile_loader.bind_mvt(ezu_features::mvt::decode(&bytes)?);

let ev = ezu_graph::Evaluator::new(&graph, &cache, &tile_loader);
let out = ev.render_parallel(
    tile_id,
    ezu_graph::CanvasInfo { tile_size: 512, pad: 24 },
    &ezu_graph::ParamValues::new(),
    /* rng_seed */ 0,
)?;
```

`build_graph` validates everything statically: every `@ref` resolves,
ports type-check, no cycles, and the required canvas padding doesn't
exceed `MAX_PAD`. Failures come back as `BuildGraphError` with the
offending node id attached.

## Evaluation

`Evaluator::render` walks the topological order sequentially.

`render_parallel` (behind the `parallel` feature) groups
nodes by their longest-path depth into level buckets — within a bucket
no two nodes share an edge, so the bucket fans out across Rayon's
global pool. Joins between buckets are cheap; the cache lookup +
hashing logic is shared with the serial path.

On a 6-node-wide watercolor graph this is the difference between
6.3 s and 1.3 s of wall-clock for 4 Tokyo tiles.

## Asset bindings

External data (images, brushes, feature layers) enters the graph
through one trait — [`AssetLoader`](src/eval.rs):

```rust
pub enum Asset {
    Image(Arc<RasterBuf>),
    Brush(OpaqueValue),
    Features(OpaqueValue),
}
pub trait AssetLoader: Send + Sync {
    fn load(&self, name: &str) -> Result<Asset, AssetError>;
    fn hash(&self, _name: &str) -> u128 { 0 }    // for cache invalidation
}
```

Think of it as **shader uniforms**: the document declares which
bindings each source node samples, the host fills the bindings, and
the evaluator stitches it together. Names beginning with `tile.` are
by convention tile-scoped — the host rebinds them per render
(`ezu-paint::host::TileLoader` is the standard overlay for that
pattern). Bare names are document-scoped (image / brush banks).

Source-style nodes declare what they sample via `Node::asset_inputs()`,
and the evaluator folds each binding's `AssetLoader::hash` into the
consuming node's cache key — change a binding's hash, every dependent
cache entry is invalidated automatically, without the node having to
remember to thread the tile id into its own `param_hash`.

## Cache

```rust
let cache = ezu_graph::Cache::with_capacity(4096);  // entries, not bytes
```

`Cache` is a Mutex-protected LRU keyed by a Merkle-style content hash:
`(canvas, tile, node param_hash + asset hashes, input hashes)` folded
into a 128-bit `xxh3`. Cloning a hit is cheap because `PortValue` wraps
the heavy variants in `Arc`. The intermediate cache is shared across
tiles within a single style, then thrown away on the next style edit.

World-anchored nodes (anything whose output is a function of world
coords — `fill-dabs`, `line`, `noise`) drop the tile id from their
cache key so adjacent tiles can share the cached result; if such a
node samples a tile-scoped asset the binding's hash brings the tile
distinction back implicitly.

## Pad propagation

A `blur(sigma=8)` node downstream of a feature source needs ~24 px of
extra border on the upstream raster to stay seamless. `Node::required_pad`
declares this growth; `Graph::compute_pad` walks the topo order in
reverse and reports the pad each source must supply.

If the requested pad exceeds `MAX_PAD` the graph rejects the build —
catches accidental "blur σ=200" before any rendering happens.

## Custom ops

```rust
struct MyOpFactory;
impl ezu_graph::NodeFactory for MyOpFactory {
    fn op_name(&self) -> &'static str { "my-op" }

    fn build(&self, fields: &serde_json::Map<String, Value>,
             _ctx: &ezu_graph::FactoryCtx<'_>)
        -> Result<ezu_graph::BuiltNode, ezu_graph::FactoryError> { /* … */ }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "description": "What I do",
            "properties": {
                "input": ezu_graph::schema_frag::node_ref(),
                "k":     ezu_graph::schema_frag::unit_number(),
            },
            "required": ["input", "k"],
        })
    }
}

// Built-in ops self-register via `ezu_graph::submit_node!(MyOpFactory)`
// at module scope, and `NodeRegistry::from_inventory()` collects them.
// For dynamic registration, hand the factory directly:
let mut registry = ezu_paint::nodes::default_registry();
registry.register(MyOpFactory);
```

The optional `schema()` method feeds `NodeRegistry::document_schema()`,
which is what `ezu-server` serves at `/schemas/ezu-style.json` for
editor autocomplete. Pre-built fragments live in
[`schema_frag`](src/registry.rs): `node_ref`, `asset_ref`, `color`,
`unit_number`, `px_number`.

## Features

- `parallel` (off by default) — pulls in Rayon and enables
  `render_parallel`. Leave off on WASM; turn on for native servers and
  CLI binaries.

## License

MIT or Apache-2.0, at your option.
