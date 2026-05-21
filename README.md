# ezu

**Painterly cartography** — render vector tiles as paintings.

`ezu` (絵図) is a Rust map rendering engine that turns vector tiles (MVT / PMTiles)
into painterly raster tiles via the [`hokusai`](https://github.com/reearth/hokusai)
brush engine and a declarative style language called **Ezu Style** (defined by the
Ezu Style Spec).

Where conventional map engines aim for cartographic accuracy, ezu aims for
**artistic interpretation** — watercolor, ink wash, ukiyo-e, and beyond — while
preserving the geographic data underneath.

## Status

Early development. The reference target is a watercolor-style map; the
Tokyo example below renders central Tokyo from the public Protomaps daily build.

## Workspace

| Crate | Description |
|---|---|
| [`ezu`](crates/ezu) | Umbrella crate (re-exports + feature flags) |
| [`ezu-core`](crates/ezu-core) | Tile / world coordinates, deterministic seeding |
| [`ezu-mvt`](crates/ezu-mvt) | MVT decoding (via `geozero`) |
| [`ezu-pmtiles`](crates/ezu-pmtiles) | PMTiles reader, local (`mmap`) and HTTP (range requests) |
| [`ezu-paint`](crates/ezu-paint) | Painting features onto a `hokusai`-backed canvas |
| [`ezu-style`](crates/ezu-style) | Ezu Style Spec parser (`serde` + `schemars`) |
| [`ezu-server`](crates/ezu-server) | Live editor + tile server (`axum`, unpublished) |
| [`ezu-wasm`](crates/ezu-wasm) | WebAssembly bindings (`wasm-bindgen`, unpublished) |

## How it paints

ezu renders each tile through three complementary primitives:

- **`fill-solid`** — `tiny-skia` solid fill plus optional outline and gaussian blur.
  Fast path for backgrounds, landuse, large patches.
- **`fill-dabs`** — `hokusai` scatter-dab fill. The polygon is rasterized to a
  binary mask, then a world-coordinate-deterministic grid of candidate positions
  is generated. Each candidate becomes a `Dab` emitted directly to a `MemSurface`
  via libmypaint's pixel kernel — no brush trajectory is constructed, and
  the same world coordinate always produces the same dab regardless of which
  tile is being rendered. That's what keeps fills seamless across tile boundaries.
- **`line`** — `hokusai::Brush::stroke_to` along a polyline. Pressure is jittered
  using a world-deterministic seed so a stroke's character is preserved across
  tile boundaries.

All painting happens on a **padded canvas** (`tile_size + 2 * pad`) so blurs
extend cleanly and MVT buffer geometry that overflows `[0, extent]` lands inside
the buffer. `encode_png()` crops back to the actual tile.

## Ezu Style — example

```json
{
  "name": "watercolor-basic",
  "version": "1",
  "tile-size": 512,
  "pad": 24,
  "background": "#f8f5e8",
  "layers": [
    { "type": "fill-solid", "id": "earth",   "source-layer": "earth",   "fill": "#f5eedc" },
    { "type": "fill-solid", "id": "landuse", "source-layer": "landuse",
      "fill": "#d6dfc5", "fill-alpha": 0.55 },

    { "type": "fill-dabs", "id": "water", "source-layer": "water",
      "color": "#5876a0", "opacity": 0.22,
      "radius-px": 7.0, "hardness": 0.5, "paint": 1.0,
      "spacing-px": 3.0, "position-jitter": 0.9,
      "size-jitter": 0.4, "opacity-jitter": 0.3, "value-jitter": 0.08 },

    { "type": "line", "id": "roads-motorway", "source-layer": "roads",
      "min-zoom-field": "min_zoom",
      "filter": { "kind_detail": "motorway" },
      "brush": "@watercolor_glazing",
      "color": "#4a3424", "radius-px": 2.6, "opacity": 0.78,
      "pressure-base": 0.85, "pressure-jitter": 0.15, "dtime": 0.04 }
  ]
}
```

Feature filters take either a single match or a list (any-of match), and may be
combined with `min-zoom-field` to drop features whose data-declared minimum
zoom is above the tile's zoom.

The full reference style ships at
[`crates/ezu/styles/watercolor-basic.json`](crates/ezu/styles/watercolor-basic.json).

## Example: render central Tokyo

The `tokyo` example fetches tiles from the public Protomaps daily build over
HTTP (range requests; no whole-archive download), decodes the MVT, and renders
the Ezu Style onto a 2×2 grid around central Tokyo:

```sh
cargo run --release -p ezu --example tokyo
# Optionally:
# cargo run --release -p ezu --example tokyo -- <STYLE.json> <YYYYMMDD> <OUT_DIR>
# EZU_TRACE=1 cargo run --release -p ezu --example tokyo
```

Brushes are loaded from `assets/brushes/` (David Revoy / MyPaint brushes,
CC0; see [`assets/brushes/CREDITS.md`](assets/brushes/CREDITS.md)). Inspecting
a tile's properties is also handy while writing styles:

```sh
cargo run --release -p ezu --example inspect -- 13 7276 3225 roads
```

## Live editor

`ezu-server` is a tiny `axum` server that ships a textarea + Leaflet split-view
editor. Edit the Ezu Style JSON on the left, click **Apply** (or `⌘↵` /
`Ctrl+↵`), and the Leaflet map on the right refreshes with the freshly rendered
tiles.

```sh
cargo run --release -p ezu-server
# then open http://127.0.0.1:8080
```

Endpoints:

| Method | Path | Description |
|---|---|---|
| `GET`  | `/` | Inline HTML editor |
| `GET`  | `/style` | Current style as raw JSON |
| `PUT`  | `/style` | Validate + replace style, returns `{ "version": N }` |
| `GET`  | `/tiles/{z}/{x}/{y}.png` | Render the tile under the current style |
| `GET`  | `/schemas/ezu-style.json` | JSON Schema for the spec |

Upstream MVT bytes are cached in process so editing the style re-renders
without hitting the PMTiles archive again.

## JSON Schema

The Ezu Style Spec is fully derivable via `schemars`. The current schema is
checked in at [`schemas/ezu-style.json`](schemas/ezu-style.json) and can be
regenerated from source:

```sh
cargo run --bin dump-schema -p ezu-style > schemas/ezu-style.json
```

The same schema is served at `/schemas/ezu-style.json` by `ezu-server`, so the
editor (or any JSON Schema-aware tool) can pick it up over HTTP for completion
and validation.

## WebAssembly

`ezu` runs in the browser via [`ezu-wasm`](crates/ezu-wasm), which exposes a
`Renderer` over `wasm-bindgen`. JS handles HTTP (PMTiles, brush files); WASM
handles parsing, painting, and PNG encoding. A self-contained demo and a
SIMD vs scalar benchmark live in that crate's README.

```sh
# Build both flavors
cd crates/ezu-wasm
wasm-pack build --target web --release --out-dir ../../target/wasm/scalar
RUSTFLAGS="-C target-feature=+simd128" \
  wasm-pack build --target web --release --out-dir ../../target/wasm/simd

# Serve the demo (also serves the editor, brushes, and /mvt)
cargo run --release -p ezu-server
# Open http://127.0.0.1:8080/wasm-demo/
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
