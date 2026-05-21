# ezu

**Painterly cartography** — render vector tiles as paintings.

`ezu` (絵図) is a Rust map rendering engine that turns vector tiles (MVT /
PMTiles) into painterly raster tiles via the
[`hokusai`](https://github.com/reearth/hokusai) brush engine and a
declarative style language called **Ezu Style**. Where conventional map
engines aim for cartographic accuracy, ezu aims for artistic
interpretation — watercolor, ink wash, ukiyo-e, and beyond — while
preserving the geographic data underneath.

## Workspace

Each crate has its own README with API details and examples.

| Crate | Description |
|---|---|
| [`ezu`](crates/ezu) | Umbrella crate, re-exports + feature flags |
| [`ezu-core`](crates/ezu-core) | Tile / world coordinates, deterministic seeding |
| [`ezu-mvt`](crates/ezu-mvt) | MVT decoding (via `geozero`) |
| [`ezu-pmtiles`](crates/ezu-pmtiles) | PMTiles reader, local (`mmap`) and HTTP (range requests) |
| [`ezu-style`](crates/ezu-style) | Style spec parser (`serde`) — pure data, no rendering |
| [`ezu-graph`](crates/ezu-graph) | Typed node-DAG evaluator (Cache, Rayon parallel) |
| [`ezu-paint`](crates/ezu-paint) | Painting primitives, built-in nodes, host glue (PNG / brush bank) |
| [`ezu-wasm`](crates/ezu-wasm) | WebAssembly bindings (`wasm-bindgen`) |
| [`ezu-server`](crates/ezu-server) | Live editor + tile server (`axum`, unpublished) |

## Try it

The `tokyo` example fetches central Tokyo tiles from the public
Protomaps daily build over HTTP and renders them under the reference
watercolor style. The `parallel` feature turns on within-tile Rayon
evaluation:

```sh
cargo run --release --features parallel -p ezu --example tokyo
# Output PNGs in ./out/tokyo/
```

The live editor (browser-based, edit JSON → see the map update):

```sh
cargo run --release -p ezu-server
# Open http://127.0.0.1:8080
```

The WASM demo (single-tile render in the browser, scalar vs SIMD switch):

```sh
# Build both flavors
cd crates/ezu-wasm
wasm-pack build --target web --release --out-dir ../../target/wasm/scalar
RUSTFLAGS="-C target-feature=+simd128" \
  wasm-pack build --target web --release --out-dir ../../target/wasm/simd

# Serve everything (editor, demo, brushes, /mvt) and open the demo
cargo run --release -p ezu-server
# http://127.0.0.1:8080/wasm-demo/
```

## How it paints

A style is a **typed node DAG**, not an ordered layer list. Every
operation is a node; ports are statically type-checked
(`Features` / `Raster` / `Mask` / `Brush` / `Scalar`); intermediate
buffers are cached and reusable across tiles.

The minimum op set ships in [`ezu-paint`](crates/ezu-paint):

- **Sources** — `solid`, `mask-solid`, `mask-circle`, `mvt-source`, `brush-file`
- **Rasterization** — `fill-solid` (tiny-skia + libblur), `fill-dabs`
  (hokusai scatter-dab fill, **world-deterministic** so dabs stay
  seamless across tile boundaries), `line` (hokusai stroke along
  polylines)
- **Composition** — `fill-with-mask`, `mask-blur`, `blend`

Example: a watercolor water layer with a brushed road on top of an
earth-tone background.

```json
{
  "name": "demo",
  "tile-size": 512,
  "pad": 24,
  "assets": { "glazing": { "type": "brush", "src": "watercolor_glazing" } },
  "nodes": {
    "bg":     { "op": "solid", "color": "#fbf6e6" },
    "earth":  { "op": "mvt-source", "source-layer": "earth" },
    "earth_p":{ "op": "fill-solid", "features": "@earth", "fill": "#e8d9b0" },
    "water":  { "op": "mvt-source", "source-layer": "water" },
    "water_p":{ "op": "fill-dabs", "features": "@water",
                "color": "#5876a0", "opacity": 0.22,
                "radius-px": 7, "spacing-px": 3 },
    "roads":  { "op": "mvt-source", "source-layer": "roads",
                "filter": { "kind_detail": "motorway" } },
    "brush":  { "op": "brush-file", "src": "@glazing" },
    "roads_p":{ "op": "line", "features": "@roads", "brush": "@brush",
                "color": "#4a3424", "radius-px": 2.6 },
    "c1":     { "op": "blend", "base": "@bg",  "over": "@earth_p" },
    "c2":     { "op": "blend", "base": "@c1",  "over": "@water_p" },
    "out":    { "op": "blend", "base": "@c2",  "over": "@roads_p" }
  },
  "output": "@out"
}
```

The full reference watercolor style is in
[`crates/ezu/examples/watercolor-basic.json`](crates/ezu/examples/watercolor-basic.json).

All painting happens on a **padded canvas** (`tile_size + 2 * pad`) so
gaussian blurs and MVT buffer geometry that overflows `[0, extent]`
land inside the buffer; the output is cropped to the tile by
[`ezu-paint::host`](crates/ezu-paint) before encoding.

## Custom ops

`NodeFactory` is a public trait — any downstream crate can register
its own ops on top of `ezu-paint::nodes::default_registry()` and feed
the registry to `ezu-graph::build_graph`. The JSON Schema served at
`/schemas/ezu-style.json` by [`ezu-server`](crates/ezu-server) is
derived from the live registry, so custom ops get editor
autocomplete out of the box.

## Brushes

The reference style consumes four CC0 watercolor brushes by David Revoy
from [`mypaint/mypaint-brushes`](https://github.com/mypaint/mypaint-brushes),
bundled under [`assets/brushes/`](assets/brushes/) with attribution in
[`assets/brushes/CREDITS.md`](assets/brushes/CREDITS.md). Any MyPaint
`.myb` brush works — point the renderer at your own.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
