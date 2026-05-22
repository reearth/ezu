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
| [`ezu-features`](crates/ezu-features) | GIS feature parsing (MVT via `geozero`, GeoJSON) — no remote fetch |
| [`ezu-style`](crates/ezu-style) | Style spec parser (`serde`) — pure data, no rendering |
| [`ezu-graph`](crates/ezu-graph) | Typed node-DAG evaluator (Cache, Rayon parallel) |
| [`ezu-paint`](crates/ezu-paint) | Painting primitives, built-in nodes, host glue (PNG / brush bank) |
| [`ezu-wasm`](crates/ezu-wasm) | WebAssembly bindings (`wasm-bindgen`) |
| [`ezu-cli`](crates/ezu-cli) | Command-line tool — `tile` / `bbox` / `tiles` rendering, `check` style validator, `serve` live editor + tile server |

## Try it

Install the CLI directly from GitHub — no clone, no `git`, just one
command:

```sh
cargo install --git https://github.com/reearth/ezu ezu-cli
```

That puts an `ezu` binary on your `PATH`. Point it at any style (URL
or local path) and any tile source (PMTiles URL/file, an `{z}/{x}/{y}`
MVT URL/path, or a TileJSON) and it spits out PNGs:

```sh
# Single tile to PNG (use `--out tile.webp` for lossless WebP)
ezu tile \
  --style https://raw.githubusercontent.com/reearth/ezu/main/crates/ezu/examples/watercolor-basic.json \
  --assets-dir ./brushes \
  --pmtiles https://build.protomaps.com/20260520.pmtiles \
  --tile 13/7276/3225 --out tile.png

# bbox mosaic — stitch the tiles covering a lon/lat box into one PNG
ezu bbox --style URL_OR_PATH --pmtiles URL_OR_PATH \
  --bbox 139.74,35.65,139.78,35.69 --zoom 13 --out tokyo.png

# XYZ pyramid — bulk-render `<out>/<z>/<x>/<y>.png` for a zoom range
ezu tiles --style URL_OR_PATH --pmtiles URL_OR_PATH \
  --bbox 139.74,35.65,139.78,35.69 \
  --min-zoom 10 --max-zoom 14 --out pyramid

# Validate a style document (parse + build graph + resolve assets).
# Exits non-zero on error — drop into a pre-commit hook / CI step.
ezu check style.json --assets-dir ./brushes
ezu check style.json --no-fetch    # parse + graph only, offline
```

The example style above references four MyPaint brushes that live in
[`assets/brushes/`](assets/brushes/) — grab them with one `curl` or
plug in your own `.myb` files and point `--assets-dir` at the folder.

For deeper hacking, clone the repo and try the `tokyo` example, which
renders a 2×2 batch under the reference watercolor style with Rayon
parallelism turned on:

```sh
cargo run --release --features parallel -p ezu --example tokyo
# Output PNGs in ./out/tokyo/
```

The live editor (browser-based, edit JSON → see the map update,
schema-validated as you type):

```sh
ezu serve
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
ezu serve
# http://127.0.0.1:8080/wasm-demo/
```

## How it paints

A style is a **typed node DAG**, not an ordered layer list. Every
operation is a node; ports are statically type-checked
(`Features` / `Raster` / `Brush` / `Scalar`); intermediate buffers are
cached and reusable across tiles.

External inputs — images, brushes, per-tile MVT/GeoJSON feature
layers — enter through one uniform `AssetLoader` trait. The style
references each binding by name (`tile.<layer>` for per-tile feature
data, bare names for document-scoped assets); the host fills the
bindings before rendering. Asset `src` entries can be local file
paths or `http(s)://` URLs — native hosts (CLI, server, examples)
prefetch URLs via `ezu_paint::host::prefetch_doc_assets` at startup
(gated behind the `http` feature). Source-format choice (MVT vs
GeoJSON vs synthesized) is a host concern, not a node concern.

The minimum op set ships in [`ezu-paint`](crates/ezu-paint):

- **Sources** — `solid`, `circle`, `noise` (white / value / perlin /
  simplex / worley, with fBm octaves and domain warp, world-anchored
  for seamless tile borders), `features`, `brush-file`
- **Rasterization** — `fill-solid` (tiny-skia + libblur), `fill-dabs`
  (hokusai scatter-dab fill, **world-deterministic** so dabs stay
  seamless across tile boundaries), `line` (hokusai stroke along
  polylines)
- **Composition** — `blur` (libblur Gaussian), `blend` (W3C 16 blend
  modes — multiply / screen / overlay / soft-light / hue / luminosity
  etc., plus `composite` operators (`destination-out` for brush-style
  eraser), `clip` for Photoshop-style clipping masks, and an optional
  alpha-`mask` input)
- **Warp** — `displace` (Photoshop-style displacement map: R/G channels
  of a second raster drive per-pixel offsets), `warp` (domain warp via
  built-in noise; world-anchored for seamless tile borders). Both grow
  upstream pad by `amp-px` and expose `clamp` / `transparent` /
  `mirror` boundary modes
- **Adjustment** — `brightness-contrast`, `hsl` (hue rotation +
  saturation/lightness shift), `invert`, `color-to-alpha` (chroma key)
- **Gradients** — `gradient-linear`, `gradient-radial` (elliptical via
  `aspect`), `gradient-conic`, `gradient-diamond`. All take color stops
  and an `anchor: "tile" | "world"` for tile-local or world-anchored
  (seamless across tiles) patterns.

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
    "earth":  { "op": "features", "name": "tile.earth" },
    "earth_p":{ "op": "fill-solid", "features": "@earth", "fill": "#e8d9b0" },
    "water":  { "op": "features", "name": "tile.water" },
    "water_p":{ "op": "fill-dabs", "features": "@water",
                "color": "#5876a0", "opacity": 0.22,
                "radius-px": 7, "spacing-px": 3 },
    "roads":  { "op": "features", "name": "tile.roads",
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
`/schemas/ezu-style.json` by `ezu serve` is derived from the live
registry, so custom ops get editor autocomplete (and as-you-type
validation in the live editor) out of the box.

## Brushes

The reference style consumes four CC0 watercolor brushes by David Revoy
from [`mypaint/mypaint-brushes`](https://github.com/mypaint/mypaint-brushes),
bundled under [`assets/brushes/`](assets/brushes/) with attribution in
[`assets/brushes/CREDITS.md`](assets/brushes/CREDITS.md). Any MyPaint
`.myb` brush works — point the renderer at your own.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
