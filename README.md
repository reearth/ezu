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

- **Painterly** — a declarative node-graph style language, **Ezu Style**,
  drives the [`hokusai`](https://github.com/reearth/hokusai) brush engine
  and a library of image-processing ops (blur, blend, warp, dither,
  gradients, …) to render watercolor, ink wash, ukiyo-e, and beyond, while
  preserving the geographic data underneath.
- **MapLibre-compatible** — `ezu translate` converts a MapLibre GL style
  into an ezu recipe, nodes carry raw MapLibre `*-expr` expression fields
  for data-driven styling, and the expression engine is the sister crate
  [`maplibre-expr`](https://github.com/reearth/maplibre-expr-rs) (100 %
  conformance against MapLibre's official spec fixtures). A Protomaps
  basemap MapLibre style renders end to end, labels included.

Full documentation — guides, the style spec, the node catalog, and the
MapLibre compatibility tables — is at
**<https://reearth.github.io/ezu/>**.

## Workspace

Each crate has its own README with API details and examples.

| Crate | crates.io | Description |
|---|---|---|
| [`ezu`](https://github.com/reearth/ezu/tree/main/crates/ezu) | [![](https://img.shields.io/crates/v/ezu.svg)](https://crates.io/crates/ezu) | Umbrella crate, re-exports + feature flags |
| [`ezu-core`](https://github.com/reearth/ezu/tree/main/crates/ezu-core) | [![](https://img.shields.io/crates/v/ezu-core.svg)](https://crates.io/crates/ezu-core) | Tile / world coordinates, deterministic seeding |
| [`ezu-features`](https://github.com/reearth/ezu/tree/main/crates/ezu-features) | [![](https://img.shields.io/crates/v/ezu-features.svg)](https://crates.io/crates/ezu-features) | GIS feature parsing (MVT via `geozero`, GeoJSON) — no remote fetch |
| [`ezu-style`](https://github.com/reearth/ezu/tree/main/crates/ezu-style) | [![](https://img.shields.io/crates/v/ezu-style.svg)](https://crates.io/crates/ezu-style) | Style spec parser (`serde`) — pure data, no rendering |
| [`ezu-graph`](https://github.com/reearth/ezu/tree/main/crates/ezu-graph) | [![](https://img.shields.io/crates/v/ezu-graph.svg)](https://crates.io/crates/ezu-graph) | Typed node-DAG evaluator (Cache, Rayon parallel) |
| [`ezu-paint`](https://github.com/reearth/ezu/tree/main/crates/ezu-paint) | [![](https://img.shields.io/crates/v/ezu-paint.svg)](https://crates.io/crates/ezu-paint) | Painting primitives, built-in nodes, host glue (PNG / asset banks / fonts) |
| [`ezu-translate`](https://github.com/reearth/ezu/tree/main/crates/ezu-translate) | [![](https://img.shields.io/crates/v/ezu-translate.svg)](https://crates.io/crates/ezu-translate) | Translate map-engine styles into ezu recipes — MapLibre GL is the first frontend |
| [`ezu-cli`](https://github.com/reearth/ezu/tree/main/crates/ezu-cli) | [![](https://img.shields.io/crates/v/ezu-cli.svg)](https://crates.io/crates/ezu-cli) | Command-line tool — `tile` / `bbox` / `tiles` rendering, `translate`, `check` validator, `graph`, `serve` live editor + tile server |
| [`ezu-wasm`](https://github.com/reearth/ezu/tree/main/crates/ezu-wasm) | — (`publish = false`) | WebAssembly bindings — scalar / SIMD / threads builds for in-browser rendering |

The expression engine lives in its own repository,
[`reearth/maplibre-expr-rs`](https://github.com/reearth/maplibre-expr-rs)
(published as [`maplibre-expr`](https://crates.io/crates/maplibre-expr)). A
`publish = false` internal benchmark, `ezu-compare`, converts a MapLibre
style, renders it with ezu, and pixel-compares against a maplibre-gl-js
reference.

## Try it

Install the CLI from crates.io:

```sh
cargo install ezu-cli
```

That puts an `ezu` binary on your `PATH`. Point it at any style (URL or
local path) and it renders PNGs. A style declares its own tile sources in
a `sources` block (MVT, PMTiles, raster DEM, RGBA raster, GeoJSON), so
most commands need nothing but a `--style` and a tile address; CLI flags
override anything declared there for one-off swaps.

The painterly reference styles reference their brushes by relative `file:`
path, so render them from a checkout (the brush files live next to the
style JSON in
[`crates/ezu/examples/styles/brushes/`](https://github.com/reearth/ezu/tree/main/crates/ezu/examples/styles/brushes/)):

```sh
git clone https://github.com/reearth/ezu && cd ezu

# Single tile to PNG (use `--out tile.webp` for lossless WebP).
ezu tile --style crates/ezu/examples/styles/watercolor.json \
  --tile 13/7276/3225 --out tile.png
```

Styles whose assets are all remote or inline need no checkout — pass a
URL directly:

```sh
# Terrain style — pulls raster DEM tiles from terrain.reearth.land.
ezu tile \
  --style https://raw.githubusercontent.com/reearth/ezu/main/crates/ezu/examples/styles/hillshade.json \
  --tile 11/1813/807 --out fuji.png

# bbox mosaic — stitch the tiles covering a lon/lat box into one PNG.
ezu bbox --style URL_OR_PATH \
  --bbox 139.74,35.65,139.78,35.69 --zoom 13 --out tokyo.png

# XYZ pyramid — bulk-render `<out>/<z>/<x>/<y>.png`, parallel across cores.
ezu tiles --style URL_OR_PATH \
  --bbox 139.74,35.65,139.78,35.69 \
  --min-zoom 10 --max-zoom 14 --out pyramid

# Validate a style (parse + build graph + resolve assets). Exits
# non-zero on error — drop into a pre-commit hook / CI step.
ezu check style.json
ezu check style.json --no-fetch    # parse + graph only, offline

# Translate a MapLibre GL style into an ezu recipe. The recipe is
# zoom-independent, so one recipe renders at every zoom; skipped or
# approximated layers are reported on stderr.
ezu translate maplibre-style.json --out recipe.json
ezu translate https://example.com/style.json | ezu check /dev/stdin --no-fetch

# `--verbose` (or `-v`) enables per-node debug logs from the evaluator.
ezu --verbose tile --style style.json --tile 13/7276/3225 --out tile.png
```

For deeper hacking, try the `tokyo` example, which renders a 2×2 batch
under the reference watercolor style with Rayon parallelism turned on:

```sh
cargo run --release --features parallel -p ezu --example tokyo
# Output PNGs in ./out/tokyo/
```

`ezu serve` starts the browser-based live editor — edit the style JSON
and watch the map update, schema-validated as you type, with generated
controls for the style's `params`:

```sh
ezu serve crates/ezu/examples/styles/pencil-sketch.json
# Open http://127.0.0.1:8080
```

The full command reference and the editor's feature list live in the
[`ezu-cli` README](https://github.com/reearth/ezu/tree/main/crates/ezu-cli).

## Features

- **Painterly node-graph styling** — a style is a typed node DAG, not an
  ordered layer list, and the painting ops drive the
  [`hokusai`](https://github.com/reearth/hokusai) brush engine:
  world-deterministic scatter-dab fills and brush strokes stay seamless
  across tile boundaries. See
  [`ezu-paint`](https://github.com/reearth/ezu/tree/main/crates/ezu-paint#how-a-style-paints).
- **~80 built-in ops** — sources, rasterization, composition, warp,
  colour adjustment, morphology, palette / dither, vector geometry,
  gradients, terrain (DEM → hillshade / slope / hypsometric tint) and
  scalar fields, all statically type-checked across seven port kinds. Full
  catalog in
  [`ezu-paint`](https://github.com/reearth/ezu/tree/main/crates/ezu-paint#built-in-nodes);
  port kinds, caching, and pad propagation in
  [`ezu-graph`](https://github.com/reearth/ezu/tree/main/crates/ezu-graph).
- **MapLibre style translation** — `ezu translate` lowers a MapLibre GL
  style into an ezu recipe: `background` / `fill` / `line` / `circle` /
  `symbol` / `raster` / `hillshade` / `heatmap` / `fill-extrusion` layers
  map onto ezu nodes, and the recipe is zoom-independent — zoom and data
  functions are emitted as raw expressions and evaluated per tile.
  Mapping table and known gaps in
  [`ezu-translate`](https://github.com/reearth/ezu/tree/main/crates/ezu-translate).
- **MapLibre expression engine** — nodes accept raw MapLibre expressions
  wherever a value varies per feature (`filter-expr`, `fill-expr`,
  `color-expr`, `width-expr`, `radius-expr`, …), hand-written or
  inherited from a translated style. They run through
  [`maplibre-expr`](https://github.com/reearth/maplibre-expr-rs), a
  pure-Rust parser/evaluator at **100 % conformance** against MapLibre's
  official spec fixtures, so evaluation matches MapLibre exactly.
- **Text labels** — MapLibre's `symbol` layer, ported: `rustybuzz`
  shaping, outline fonts or MapLibre glyph-PBF endpoints, point / line
  placement, SDF glyphs with expression-driven paint. Collision is
  **deterministic across tile boundaries** and shared across every label
  layer, with icons placing as one unit with their text
  (`icon-text-fit` included). See
  [`ezu-paint`](https://github.com/reearth/ezu/tree/main/crates/ezu-paint#text-labels).
- **Parametric styles** — a `params` block declares typed knobs that
  resolve at render time, so one built graph serves every combination;
  override them per render from the CLI, a query string, or a library
  call. Styles also factor repeated patterns into user-defined
  functions. Both in
  [`ezu-style`](https://github.com/reearth/ezu/tree/main/crates/ezu-style#params).
- **Custom ops** — `NodeFactory` is public: register your own ops on top
  of the built-in registry and they inherit the served JSON Schema, so
  editor autocomplete and live validation come for free. See
  [`ezu-graph`](https://github.com/reearth/ezu/tree/main/crates/ezu-graph#custom-ops).
- **Brushes** — nothing is bundled; a style names every brush through its
  `sources` block and any MyPaint `.myb` file works. The example styles
  ship CC0 brushes by David Revoy alongside their style JSON. See
  [`ezu-paint`](https://github.com/reearth/ezu/tree/main/crates/ezu-paint#brushes).
- **WASM + live editor** — the renderer compiles to WebAssembly for
  in-browser rendering, and `ezu serve` hosts a live style editor with a
  tile server. See [WASM](#wasm) and the
  [`ezu-cli` README](https://github.com/reearth/ezu/tree/main/crates/ezu-cli).

## Performance

ezu renders on the CPU with a cache-aware, optionally Rayon-parallel
evaluator. On an **Apple M1** (4 performance + 4 efficiency cores), a
Protomaps basemap MapLibre style converted with `ezu translate` and
rendered at **512 px** evaluates in **~13–30 ms per tile single-threaded**
across z12–z15 — pure graph evaluation, MVT fetch/decode excluded:

```sh
cargo run --release -p ezu-compare -- \
  --style crates/ezu-compare/samples/protomaps-basemap.json \
  --tiles 12/3637/1613,13/7275/3225,14/14550/6452,15/29101/12904 \
  --bench --repeat 5
# eval: z12 ~22 ms, z13 ~30 ms, z14 ~19 ms, z15 ~13 ms
```

End to end, a 251-tile z13–z14 pyramid (`ezu tiles`, parallel evaluator
across all 8 cores) renders in **~9 s wall** — ~37 ms/tile amortized
*including* HTTP tile fetch, MVT decode, and PNG encode, which dominate
the per-tile wall at this render cost. Numbers are from this machine on
the sample style; your mileage varies with hardware, style complexity,
and network.

## WASM

[`ezu-wasm`](https://github.com/reearth/ezu/tree/main/crates/ezu-wasm)
compiles the renderer to WebAssembly for in-browser rendering. The JS
side owns all I/O and supplies decoded bytes — MVT/PMTiles tiles, brushes,
images, fonts, DEM/raster neighbours — to a stateful `Renderer` through
`bindSource`; `renderTile` returns PNG, lossless WebP, or raw RGBA (blit
straight to a `<canvas>` via `putImageData`). Three builds sit side by
side under `target/wasm/`: **scalar** and **SIMD** (`+simd128`) on stable
Rust, plus a **threads** build that renders across Web Workers via
[`wasm-bindgen-rayon`](https://github.com/RReverser/wasm-bindgen-rayon)
(nightly to build, cross-origin-isolated page to run). A self-contained
demo page and the routes it needs ship with `ezu serve`. See the
[`ezu-wasm` README](https://github.com/reearth/ezu/tree/main/crates/ezu-wasm)
for the full JS API, build commands, and benchmarks.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
