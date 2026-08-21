# ezu

[![Crates.io](https://img.shields.io/crates/v/ezu.svg)](https://crates.io/crates/ezu)
[![docs.rs](https://img.shields.io/docsrs/ezu)](https://docs.rs/ezu)
[![CI](https://github.com/reearth/ezu/actions/workflows/ci.yml/badge.svg)](https://github.com/reearth/ezu/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Docs](https://img.shields.io/badge/docs-reearth.github.io%2Fezu-blue)](https://reearth.github.io/ezu/)

**Painterly cartography** — render vector tiles as paintings — on a
**pure-Rust, GPU-free CPU renderer** with **first-class MapLibre
compatibility**.

![ezu pencil-sketch render of central Japan — © OpenStreetMap contributors, © Protomaps](docs/src/assets/hero.webp)

`ezu` (絵図) is a Rust map rendering engine that turns vector tiles (MVT /
PMTiles) into raster tiles on the CPU — no GPU, no headless browser. It
does this two ways, and does both at once:

- **Painterly** — a style is a typed node graph, not an ordered layer list.
  It drives the [`hokusai`](https://github.com/reearth/hokusai) brush engine
  and ~80 image-processing ops to render watercolour, ink wash, ukiyo-e and
  beyond, with the geographic data intact underneath. Dab placement and
  label collision are deterministic in world space, so tile borders don't
  show.
- **MapLibre-compatible** — `ezu translate` lowers a MapLibre GL style into
  an ezu recipe, and any node field that varies per feature takes a raw
  MapLibre expression, evaluated by
  [`maplibre-expr`](https://github.com/reearth/maplibre-expr-rs) at 100 %
  conformance against MapLibre's official spec fixtures. A 68-layer
  Protomaps theme renders end to end, labels and icons included, at
  [SSIM 0.80–0.87](https://reearth.github.io/ezu/maplibre/compatibility/)
  against maplibre-gl-js.

## Documentation

**<https://reearth.github.io/ezu/>**

| | |
|---|---|
| [Render your first tile](https://reearth.github.io/ezu/guides/first-tile/) | from a flat colour to a watercolour wash, one node at a time |
| [Guides](https://reearth.github.io/ezu/guides/what-is-ezu/) | the CLI, the live editor, the Rust library, the browser, serving tiles |
| [Concepts](https://reearth.github.io/ezu/concepts/node-graph/) | the node graph, ports and types, determinism, padding, caching |
| [Style reference](https://reearth.github.io/ezu/style/overview/) | the spec, and a catalog of all 82 ops with a render of each |
| [MapLibre compatibility](https://reearth.github.io/ezu/maplibre/compatibility/) | layer mapping, measured fidelity, and the known gaps |
| [Gallery](https://reearth.github.io/ezu/gallery/) | what the example styles look like, with the recipes |

Rust API documentation is on [docs.rs](https://docs.rs/ezu).

## Quick start

```sh
cargo install ezu-cli
```

Then render a tile. This style keeps all of its data remote, so it needs
nothing but the CLI:

```sh
ezu tile \
  --style https://raw.githubusercontent.com/reearth/ezu/main/crates/ezu/examples/styles/hillshade.json \
  --tile 11/1813/807 --out fuji.png
```

The painterly example styles name their brushes by relative `file:` path,
so render those from a checkout:

```sh
git clone https://github.com/reearth/ezu && cd ezu
ezu tile --style crates/ezu/examples/styles/watercolor.json \
  --tile 13/7276/3225 --out tile.png
```

`ezu serve` starts the live editor — edit the style and watch the map
redraw, schema-validated as you type, with generated controls for the
style's `params`:

```sh
ezu serve crates/ezu/examples/styles/pencil-sketch.json
# → http://127.0.0.1:8080
```

`ezu bbox` stitches a lon/lat box into one image, `ezu tiles` bulk-renders
an XYZ pyramid, `ezu translate` converts a MapLibre style, `ezu check`
validates one, and `ezu schema` prints the style JSON Schema. Full
reference: [CLI](https://reearth.github.io/ezu/reference/cli/).

## Workspace

| Crate | crates.io | Description |
|---|---|---|
| [`ezu`](https://github.com/reearth/ezu/tree/main/crates/ezu) | [![](https://img.shields.io/crates/v/ezu.svg)](https://crates.io/crates/ezu) | Umbrella crate, re-exports + feature flags |
| [`ezu-core`](https://github.com/reearth/ezu/tree/main/crates/ezu-core) | [![](https://img.shields.io/crates/v/ezu-core.svg)](https://crates.io/crates/ezu-core) | Tile / world coordinates, deterministic seeding |
| [`ezu-features`](https://github.com/reearth/ezu/tree/main/crates/ezu-features) | [![](https://img.shields.io/crates/v/ezu-features.svg)](https://crates.io/crates/ezu-features) | GIS feature parsing (MVT via `geozero`, GeoJSON) — no remote fetch |
| [`ezu-style`](https://github.com/reearth/ezu/tree/main/crates/ezu-style) | [![](https://img.shields.io/crates/v/ezu-style.svg)](https://crates.io/crates/ezu-style) | Style spec parser (`serde`) — pure data, no rendering |
| [`ezu-graph`](https://github.com/reearth/ezu/tree/main/crates/ezu-graph) | [![](https://img.shields.io/crates/v/ezu-graph.svg)](https://crates.io/crates/ezu-graph) | Typed node-DAG evaluator (cache, pad propagation, Rayon) |
| [`ezu-paint`](https://github.com/reearth/ezu/tree/main/crates/ezu-paint) | [![](https://img.shields.io/crates/v/ezu-paint.svg)](https://crates.io/crates/ezu-paint) | Painting primitives, the built-in ops, host glue |
| [`ezu-translate`](https://github.com/reearth/ezu/tree/main/crates/ezu-translate) | [![](https://img.shields.io/crates/v/ezu-translate.svg)](https://crates.io/crates/ezu-translate) | Lower other engines' styles into ezu recipes — MapLibre GL is the first frontend |
| [`ezu-cli`](https://github.com/reearth/ezu/tree/main/crates/ezu-cli) | [![](https://img.shields.io/crates/v/ezu-cli.svg)](https://crates.io/crates/ezu-cli) | The `ezu` binary — rendering, `translate`, `check`, `graph`, `schema`, `serve` |
| [`ezu-wasm`](https://github.com/reearth/ezu/tree/main/crates/ezu-wasm) | [npm](https://www.npmjs.com/package/@reearth/ezu) | WebAssembly bindings — scalar / SIMD / threads builds for in-browser rendering |

The expression engine lives in its own repository,
[`reearth/maplibre-expr-rs`](https://github.com/reearth/maplibre-expr-rs).
`ezu-compare` (internal, unpublished) converts a MapLibre style, renders it
with ezu, and pixel-compares against a maplibre-gl-js reference.

## Performance

On an Apple M1, a converted Protomaps basemap at 512 px evaluates in
**~13–30 ms per tile** single-threaded across z12–z15, and a full 68-layer
theme with labels in **~42–105 ms**. End to end, a 251-tile z13–z14 pyramid
renders in **~9 s** across 8 cores — ~37 ms/tile including HTTP fetch, MVT
decode and PNG encode.

A style's *first* render is far slower than that, because lazily fetched
glyphs and neighbour tiles land inside evaluation. Methodology, the per-op
breakdown, and what to tune are in
[performance](https://reearth.github.io/ezu/performance/benchmarks/).

## In the browser

```sh
npm install @reearth/ezu
```

The renderer compiles to WebAssembly, in scalar, SIMD and Web-Worker
threads builds. The JS side owns all I/O and hands decoded bytes to a
stateful `Renderer`, which returns PNG, lossless WebP, or raw RGBA to blit
onto a canvas. See
[use in the browser](https://reearth.github.io/ezu/guides/browser-wasm/).

## Contributing

Repository layout, how the tests are structured, and how the docs site's
generated pages and images are produced:
[contributing](https://reearth.github.io/ezu/reference/contributing/).
Bugs and design questions are both welcome as
[issues](https://github.com/reearth/ezu/issues).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
