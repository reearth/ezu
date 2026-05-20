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
| [`ezu-style`](crates/ezu-style) | Ezu Style Spec parser (`serde`) |

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

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
