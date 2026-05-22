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

## Types

```rust
pub struct Document {
    pub name: String,
    pub tile_size: u32,
    pub pad: u32,
    pub params: IndexMap<String, ParamDecl>,
    pub assets: IndexMap<String, AssetDecl>,
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
