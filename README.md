# ezu

[![Crates.io](https://img.shields.io/crates/v/ezu.svg)](https://crates.io/crates/ezu)
[![docs.rs](https://img.shields.io/docsrs/ezu)](https://docs.rs/ezu)
[![CI](https://github.com/reearth/ezu/actions/workflows/ci.yml/badge.svg)](https://github.com/reearth/ezu/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

**Painterly cartography** — render vector tiles as paintings.

![ezu pencil-sketch render of central Japan — © OpenStreetMap contributors, © Protomaps](docs/hero.webp)

`ezu` (絵図) is a Rust map rendering engine that turns vector tiles (MVT /
PMTiles) into painterly raster tiles via the
[`hokusai`](https://github.com/reearth/hokusai) brush engine and a
declarative style language called **Ezu Style**. Where conventional map
engines aim for cartographic accuracy, ezu aims for artistic
interpretation — watercolor, ink wash, ukiyo-e, and beyond — while
preserving the geographic data underneath.

## Workspace

Each crate has its own README with API details and examples.

| Crate | crates.io | Description |
|---|---|---|
| [`ezu`](https://github.com/reearth/ezu/tree/main/crates/ezu) | [![](https://img.shields.io/crates/v/ezu.svg)](https://crates.io/crates/ezu) | Umbrella crate, re-exports + feature flags |
| [`ezu-core`](https://github.com/reearth/ezu/tree/main/crates/ezu-core) | [![](https://img.shields.io/crates/v/ezu-core.svg)](https://crates.io/crates/ezu-core) | Tile / world coordinates, deterministic seeding |
| [`ezu-features`](https://github.com/reearth/ezu/tree/main/crates/ezu-features) | [![](https://img.shields.io/crates/v/ezu-features.svg)](https://crates.io/crates/ezu-features) | GIS feature parsing (MVT via `geozero`, GeoJSON) — no remote fetch |
| [`ezu-style`](https://github.com/reearth/ezu/tree/main/crates/ezu-style) | [![](https://img.shields.io/crates/v/ezu-style.svg)](https://crates.io/crates/ezu-style) | Style spec parser (`serde`) — pure data, no rendering |
| [`ezu-graph`](https://github.com/reearth/ezu/tree/main/crates/ezu-graph) | [![](https://img.shields.io/crates/v/ezu-graph.svg)](https://crates.io/crates/ezu-graph) | Typed node-DAG evaluator (Cache, Rayon parallel) |
| [`ezu-paint`](https://github.com/reearth/ezu/tree/main/crates/ezu-paint) | [![](https://img.shields.io/crates/v/ezu-paint.svg)](https://crates.io/crates/ezu-paint) | Painting primitives, built-in nodes, host glue (PNG / brush bank) |
| [`ezu-cli`](https://github.com/reearth/ezu/tree/main/crates/ezu-cli) | [![](https://img.shields.io/crates/v/ezu-cli.svg)](https://crates.io/crates/ezu-cli) | Command-line tool — `tile` / `bbox` / `tiles` rendering, `check` style validator, `serve` live editor + tile server |
| [`ezu-translate`](https://github.com/reearth/ezu/tree/main/crates/ezu-translate) | [![](https://img.shields.io/crates/v/ezu-translate.svg)](https://crates.io/crates/ezu-translate) | Translate map-engine styles into ezu recipes — MapLibre GL is the first frontend |

## Try it

Install the CLI from crates.io:

```sh
cargo install ezu-cli
```

That puts an `ezu` binary on your `PATH`. Point it at any style (URL
or local path) and a tile source (PMTiles URL/file, an `{z}/{x}/{y}`
MVT URL/path, or a TileJSON) and it spits out PNGs. The style can
declare its own tile sources in a `sources` block (MVT, PMTiles, or
raster DEM); CLI flags override anything declared there for one-off
swaps:

```sh
# Single tile to PNG (use `--out tile.webp` for lossless WebP). The
# reference styles bundle their own `sources` block (Protomaps daily
# build + Re:Earth Terrain), so no `--pmtiles` / `--mvt` is needed —
# pass them to override what the style declares.
ezu tile \
  --style https://raw.githubusercontent.com/reearth/ezu/main/crates/ezu/examples/styles/watercolor.json \
  --tile 13/7276/3225 --out tile.png

# Terrain style — pulls raster DEM tiles from terrain.reearth.land.
ezu tile --style crates/ezu/examples/styles/hillshade.json \
  --tile 11/1813/807 --out fuji.png

# bbox mosaic — stitch the tiles covering a lon/lat box into one PNG.
ezu bbox --style URL_OR_PATH \
  --bbox 139.74,35.65,139.78,35.69 --zoom 13 --out tokyo.png

# XYZ pyramid — bulk-render `<out>/<z>/<x>/<y>.png` for a zoom range.
ezu tiles --style URL_OR_PATH \
  --bbox 139.74,35.65,139.78,35.69 \
  --min-zoom 10 --max-zoom 14 --out pyramid

# Validate a style document (parse + build graph + resolve assets).
# Exits non-zero on error — drop into a pre-commit hook / CI step.
ezu check style.json
ezu check style.json --no-fetch    # parse + graph only, offline

# Translate a MapLibre GL style into an ezu recipe (via ezu-translate).
# The recipe is zoom-independent: zoom/data functions are emitted as raw
# expressions and evaluated per tile, so one recipe renders at every zoom.
# Skipped/approximated layers are reported on stderr. Writes to stdout
# without `--out`.
ezu translate maplibre-style.json --out recipe.json
ezu translate https://example.com/style.json | ezu check /dev/stdin --no-fetch

# `--verbose` (or `-v`) enables per-node debug logs from the
# evaluator: op name, cache hit/miss, output shape, eval duration.
ezu --verbose tile --style style.json --tile 13/7276/3225 --out tile.png
```

The reference style references brushes by name (`watercolor_glazing`,
`2B_pencil`, …) — these are CC0 MyPaint brushes bundled into the binary,
so they resolve without any host-side file staging. To bring your own
`.myb` brush, declare it in the style's `assets` block (with an
`http(s)://` URL or a path relative to `--assets-dir`).

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
ezu serve                          # default example style
ezu serve crates/ezu/examples/styles/pencil-sketch.json  # open a specific style
ezu serve https://example.com/style.json          # or fetch one over http(s)
# Open http://127.0.0.1:8080
```

The editor (MapLibre GL based) supports:

- **Open / URL / Save** — load a style from a local file or http(s) URL,
  save the current buffer as `<name>.json`. Open on Chromium browsers
  uses the File System Access API so Save writes back in place.
- **Apply** with `⌘↵` / `Ctrl+↵` (works anywhere on the page).
- **Live preview** — when enabled, auto-applies on every keystroke that
  parses + schema-validates + server-validates clean.
- **External-edit reload** — when launched with a local path
  (`ezu serve foo.json`), the server polls the file and pushes
  Server-Sent Events on every change. The editor swaps the buffer
  silently when clean, or surfaces a Reload banner when the user has
  unsaved edits. The `↻ HH:MM:SS` indicator in the toolbar shows the
  last auto-reload. On Chromium, the same watch also runs against
  files opened via the in-browser file picker. Opening a different
  file via `Open…` / `URL…` detaches the server watch for that
  session.
- **Params panel** — controls generated from the style's `params`
  declarations (sliders for bounded numbers, color pickers, toggles).
  Adjustments ride the tile requests as query-string overrides and
  re-render live without touching the style text; `reset` returns to
  the declared defaults.
- **Source MVT inspector** — toggle a vector overlay of the underlying
  MVT, with per-layer ON/OFF and click-to-inspect feature properties.
  Layers are discovered from the tile at the map center; pan/zoom
  rescans automatically.
- **Tile grid + zoom indicator** — toggle a `z/x/y` boundary overlay
  (drawn per tile via `maplibregl.addProtocol`), and read the live
  zoom value (click to copy `z @ lat,lng`).

## How it paints

A style is a **typed node DAG**, not an ordered layer list. Every
operation is a node; ports are statically type-checked across six
kinds — `Features` (geometry + props), `Raster` (canvas-sized RGBA),
`Sprite` (image at native dimensions, consumed by placement ops),
`Brush` (hokusai brush handle), `Scalar` (constants),
`ScalarField` (per-pixel `f32` grid — elevation, distance, scalar
noise; carries optional geographic scaling). Ports list the kinds
they accept, so polymorphic ops (e.g. `blur` over `Raster`/`Sprite`)
pass the input kind straight through. Intermediate buffers are
cached and reusable across tiles.

Tile-pyramid inputs go beyond vectors: `dem` sources feed elevation
as a `ScalarField`, and `raster` sources feed RGBA imagery (XYZ /
TileJSON / PMTiles; satellite photos, pre-rendered basemaps) as a
seam-free padded `Raster` — so any filter chain can post-process a
photo basemap (`photo-pop.json` posterizes 国土地理院 aerial
imagery). Styles and sources declare `attribution`, and sources
without one inherit upstream TileJSON / PMTiles metadata; the live
editor's MapLibre attribution control updates from
`GET /style/attribution` automatically.

External inputs — images, brushes, per-tile MVT/GeoJSON feature
layers — enter through one uniform `AssetLoader` trait. The style's
`features` node references its layer by `(source, layer)` pointing
into the document's `sources` block; the host binds decoded data
under `<source>.<layer>` per tile before rendering. Document-scoped
assets (brushes, images) use `scheme:`-prefixed names (`builtin:`,
`file:`, `http(s)://`). Asset `src` entries can be local file
paths or `http(s)://` URLs — native hosts (CLI, server, examples)
prefetch URLs via `ezu_paint::host::prefetch_doc_assets` at startup
(gated behind the `http` feature). Source-format choice (MVT vs
GeoJSON vs synthesized) is a host concern, not a node concern.

The minimum op set ships in [`ezu-paint`](https://github.com/reearth/ezu/tree/main/crates/ezu-paint):

- **Sources** — `solid`, `circle` (both with optional `kind: sprite`
  for synthetic placement/tiling source), `noise` (white / value /
  perlin / simplex / worley, with fBm octaves and domain warp,
  world-anchored for seamless tile borders; `kind: scalar` emits raw
  fBm as a `ScalarField` for terrain stylization), `features`,
  `brush-file`, `image` (load a PNG/WebP asset as a `Sprite` for
  placement / tiling ops)
- **Rasterization** — `fill-solid` (tiny-skia + libblur), `fill-dabs`
  (hokusai scatter-dab fill, **world-deterministic** so dabs stay
  seamless across tile boundaries), `line` (hokusai brush stroke along
  polylines), `stroke` (crisp constant-width tiny-skia vector stroke with
  cap/join + optional `dasharray` — clean cartographic lines), `stamp`
  (paint an image per feature point — accepts a
  `Sprite` or canvas-sized `Raster`), `place` (composite one image at
  fixed canvas coordinates with `fit: none/cover/contain/stretch`),
  `tiling` (repeat an image across the canvas, world-anchored for
  seamless tile borders)
- **Composition** — `blur` (libblur Gaussian), `blend` (W3C 16 blend
  modes — multiply / screen / overlay / soft-light / hue / luminosity
  etc., plus `composite` operators (`destination-out` for brush-style
  eraser), `clip` for Photoshop-style clipping masks, and an optional
  alpha-`mask` input), `mix` (tween two rasters by a scalar `t` in a
  selectable colour `space` — a straight colour blend, not a composite)
- **Warp** — `displace` (Photoshop-style displacement map: R/G channels
  of a second raster drive per-pixel offsets), `warp` (domain warp via
  built-in noise; world-anchored for seamless tile borders). Both grow
  upstream pad by `amp-px` and expose `clamp` / `transparent` /
  `mirror` boundary modes
- **Adjustment** — `brightness-contrast`, `levels` (Photoshop-style
  in/out black/white + gamma), `hsl` (hue rotation +
  saturation/lightness shift), `saturate` / `vibrance` (scale CIELAB
  chroma — uniform, or adaptively boosting low-chroma pixels — preserving
  hue + lightness), `invert`, `color-to-alpha` (chroma key)
- **Morphology / edges** — `erode` / `dilate` (per-channel min/max box
  filter, for mask cleanup), `edge-detect` (Sobel gradient magnitude),
  `sharpen` (4-neighbour Laplacian)
- **Channel ops** — `channel-shuffle` (rearrange RGBA, or stamp
  constants `0` / `1` into channels), `posterize` (per-channel
  quantisation), `quantize` (snap to a fixed palette by nearest colour in
  perceptual CIELAB or RGB — limited-palette / poster / pixel-art looks),
  `dither` (palette reduction with Floyd–Steinberg error diffusion or an
  ordered Bayer matrix — retro / print looks)
- **Geometry (Voronoi family)** — `voronoi` (point set → diagram
  edges), `voronoi-fracture` (split polygons into Voronoi sub-cells
  via seed points), `medial-axis` (polygon → skeleton polylines for
  river / lake centrelines and similar), `triangulate` (Delaunay)
- **Geometry (set + transform)** — `feature-boolean`
  (union / intersection / difference / xor over polygons),
  `transform` (translate / rotate / scale), `bbox` (axis-aligned
  envelope), `smooth` (Chaikin), `densify`, `resample`
- **Utility** — `switch` (build-time A/B selection over any port
  kind; great for param-driven variants), `pick-channel` (extract
  R/G/B/A/luminance from a Raster as a `ScalarField`, bridging into
  `map-range` / `threshold` / `color-ramp`)
- **Scalar math** — `map-range` (linear remap with optional clamp on a
  `ScalarField`), `threshold` (binarise with optional soft ramp)
- **Gradients** — `gradient-linear`, `gradient-radial` (elliptical via
  `aspect`), `gradient-conic`, `gradient-diamond`. All take color stops
  and an `anchor: "tile" | "world"` for tile-local or world-anchored
  (seamless across tiles) patterns. Stops interpolate in a selectable
  `space` (`rgb` default, plus `hsl` / `hsv` / `hcl` / `lab`; hue-based
  spaces take the shortest path around the wheel).
- **Terrain** — `dem` (sample a host-bound raster-DEM mosaic as a
  `ScalarField` with `geo_scale` populated; the host declares the tile pyramid in `sources` and
  handles fetch / decode / 3×3 stitch / overzoom upsampling for
  terrarium and mapbox-rgb encodings), `hillshade` (Horn-method
  analytical shade with `shade` or multiply-friendly `relief` mode —
  `relief` takes an optional `shadow-color` / `highlight-color` (à la
  MapLibre) — optional ESRI multidirectional), `slope`, `color-ramp` (any scalar
  field → colour via a stops table, with a selectable interpolation
  `space` — `rgb` / `hsl` / `hsv` / `hcl` / `lab`; canonical use is
  hypsometric tinting of a DEM).

Example: a watercolor water layer with a brushed road on top of an
earth-tone background.

```json
{
  "name": "demo",
  "tile-size": 512,
  "pad": 24,
  "sources": {
    "glazing":  { "type": "brush", "src": "file:brushes/watercolor_glazing.myb" },
    "basemap":  { "type": "mvt", "url": "https://papers.reearth.land/protomaps/tilejson.json" }
  },
  "nodes": {
    "bg":     { "op": "solid", "color": "#fbf6e6" },
    "earth":  { "op": "features", "layer": "earth" },
    "earth_p":{ "op": "fill-solid", "features": "@earth", "fill": "#e8d9b0" },
    "water":  { "op": "features", "layer": "water" },
    "water_p":{ "op": "fill-dabs", "features": "@water",
                "color": "#5876a0", "opacity": 0.22,
                "radius-px": 7, "spacing-px": 3 },
    "roads":  { "op": "features", "layer": "roads",
                "filter-expr": ["==", ["get", "kind_detail"], "motorway"] },
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
[`crates/ezu/examples/styles/watercolor.json`](https://github.com/reearth/ezu/tree/main/crates/ezu/examples/styles/watercolor.json).

All painting happens on a **padded canvas** (`tile_size + 2 * pad`) so
gaussian blurs and MVT buffer geometry that overflows `[0, extent]`
land inside the buffer; the output is cropped to the tile by
[`ezu-paint::host`](https://github.com/reearth/ezu/tree/main/crates/ezu-paint) before encoding.

## Parametric styles

A style can declare typed parameters in a `params` block and reference
them anywhere a scalar field lives, with `$name`:

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

Parameters resolve at render time — the same built graph serves every
parameter combination, and the intermediate cache keys on the values a
node actually reads, so flipping one param only re-evaluates the nodes
that depend on it. Override them per render:

```sh
# CLI: repeatable --param flags, validated against the declarations.
ezu tile --style watercolor.json --tile 13/7276/3225   --param 'paper=#ffe0f0' --param softness=2 --out tile.png

# Tile server: query-string overrides on the tile endpoint.
curl 'http://127.0.0.1:8080/tiles/13/7276/3225.png?paper=%23ffe0f0&softness=2'

# JSON Schema for the current style's parameters (defaults, ranges,
# descriptions) — drive sliders / color pickers off this.
curl http://127.0.0.1:8080/style/params
```

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

One constraint: fields that decide canvas padding at build time (blur
sigmas and friends) need a static upper bound — a literal, or a
`$param` with `max` declared. Wiring those from a `@node` port is a
build error.

See `crates/ezu/examples/styles/watercolor.json` for a complete
parametric style.

Styles can also factor repeated node patterns into **user-defined
functions** — reusable subgraphs with typed input ports, called with
`op: "func"` and expanded inline at build time. The full semantics
(argument substitution, hygiene, recursion errors) live in the
[`ezu-style` README](https://github.com/reearth/ezu/tree/main/crates/ezu-style#functions);
`pencil-sketch.json` shows them in action.

## Custom ops

`NodeFactory` is a public trait — any downstream crate can register
its own ops on top of `ezu-paint::nodes::default_registry()` and feed
the registry to `ezu-graph::build_graph`. The JSON Schema served at
`/schemas/ezu-style.json` by `ezu serve` is derived from the live
registry, so custom ops get editor autocomplete (and as-you-type
validation in the live editor) out of the box.

## Brushes

Nothing is bundled into the library — a style references every brush it
uses through a `src` in its `sources` block. The example styles ship
their brushes alongside the style JSON in
[`crates/ezu/examples/styles/brushes/`](https://github.com/reearth/ezu/tree/main/crates/ezu/examples/styles/brushes/)
and reference them by relative `file:` path (resolved against the style
file's directory); those are CC0 brushes by David Revoy from
[`mypaint/mypaint-brushes`](https://github.com/mypaint/mypaint-brushes)
(attribution in
[`brushes/CREDITS.md`](https://github.com/reearth/ezu/tree/main/crates/ezu/examples/styles/brushes/CREDITS.md)).
Any MyPaint `.myb` brush works — declare it in the style's `sources`
block and the host loads it from disk or HTTP.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
