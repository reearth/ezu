# ezu

**Painterly cartography** — render vector tiles as paintings.

`ezu` (絵図) is a Rust map rendering engine that turns vector tiles (MVT /
PMTiles) into painterly raster tiles via the
[`hokusai`](https://github.com/reearth/hokusai) brush engine and a
declarative style language called **Ezu Style** (defined by the Ezu Style
Spec). Where conventional map engines aim for cartographic accuracy, ezu
aims for artistic interpretation — watercolor, ink wash, ukiyo-e, and
beyond — while preserving the geographic data underneath.

## Workspace

Each crate has its own README with API details and examples.

| Crate | Description |
|---|---|
| [`ezu`](crates/ezu) | Umbrella crate, re-exports + feature flags |
| [`ezu-core`](crates/ezu-core) | Tile / world coordinates, deterministic seeding |
| [`ezu-mvt`](crates/ezu-mvt) | MVT decoding (via `geozero`) |
| [`ezu-pmtiles`](crates/ezu-pmtiles) | PMTiles reader, local (`mmap`) and HTTP (range requests) |
| [`ezu-paint`](crates/ezu-paint) | Painting features onto a `hokusai`-backed canvas |
| [`ezu-style`](crates/ezu-style) | Ezu Style Spec parser (`serde` + `schemars`) |
| [`ezu-wasm`](crates/ezu-wasm) | WebAssembly bindings (`wasm-bindgen`) |
| [`ezu-server`](crates/ezu-server) | Live editor + tile server (`axum`, unpublished) |

## Try it

The `tokyo` example fetches central Tokyo tiles from the public Protomaps
daily build over HTTP and renders them under the reference watercolor style:

```sh
cargo run --release -p ezu --example tokyo
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

Three complementary primitives, all dispatched declaratively from an
Ezu Style document (see [`crates/ezu-style`](crates/ezu-style/README.md)):

- **`fill-solid`** — `tiny-skia` solid fill + optional outline +
  `libblur` gaussian blur. Fast path for backgrounds and large patches.
- **`fill-dabs`** — `hokusai` scatter-dab fill with
  world-coordinate-deterministic jitter. Same world location always
  emits the same dab, regardless of which tile it lives on — that's
  what keeps fills seamless across tile boundaries.
- **`line`** — `hokusai::Brush::stroke_to` along a polyline with
  world-deterministic pressure jitter.

All painting happens on a **padded canvas** (`tile_size + 2 * pad`) so
blurs extend cleanly and MVT buffer geometry that overflows `[0, extent]`
lands inside the buffer. The output is cropped to the actual tile.

More design notes live in the per-crate READMEs:
[`ezu-paint`](crates/ezu-paint/README.md) for the rendering primitives,
[`ezu-style`](crates/ezu-style/README.md) for the spec,
[`ezu-wasm`](crates/ezu-wasm/README.md) for the WASM build + SIMD bench.

## Brushes

The reference style consumes four CC0 watercolor brushes by David Revoy
from [`mypaint/mypaint-brushes`](https://github.com/mypaint/mypaint-brushes),
bundled under [`assets/brushes/`](assets/brushes/) with attribution in
[`assets/brushes/CREDITS.md`](assets/brushes/CREDITS.md). Any MyPaint
`.myb` brush works — point the renderer at your own.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
