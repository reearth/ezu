# ezu-paint

Rendering primitives + built-in node implementations for the
[`ezu`](../../README.md) workspace.

This crate sits between the low-level brush engine
([`hokusai`](https://github.com/reearth/hokusai)) / 2D rasterizer
(`tiny-skia`) / blur (`libblur`) and the graph evaluator
([`ezu-graph`](../ezu-graph)).

Three things live here:

1. **Paint primitives** — functions that take a `Canvas` and feature
   data and produce pixels. Reusable on their own.
2. **`nodes` module** — `NodeFactory` implementations for each built-in
   op, grouped into `raster`, `source`, `paint`, `geometry`
   submodules. Each op self-registers via `ezu_graph::submit_node!`;
   `default_registry()` just collects everything via
   `NodeRegistry::from_inventory()`.
3. **`host` module** — host-side glue: ready-made `AssetLoader`
   implementations (`BrushBankLoader` for document-scoped images /
   brushes, `TileLoader` for per-tile feature overlays), and
   conversions from `RasterBuf` to PNG / straight RGBA.

## Paint primitives

| Function | Op name | What it does |
|---|---|---|
| `paint_polygons` | `fill-solid` | `tiny-skia` solid fill + optional outline + `libblur` gaussian blur |
| `paint_polygons_dabs` | `fill-dabs` | `hokusai` scatter-dab fill with **world-deterministic** position / size / opacity jitter — same world coord → same dab regardless of tile |
| `paint_lines` | `line` | `hokusai::Brush::stroke_to` per polyline vertex with world-seeded pressure jitter |

For `fill-dabs` the polygon is rasterized to a binary mask, then a
regular grid of candidate positions is iterated; no brush trajectory is
constructed, which is what keeps fills seamless across tile boundaries.

## Stroke curves on `line`

`line` exposes four optional **stroke curves** that vary brush
behavior along each polyline, so strokes can simulate taper-in /
taper-out and speed dynamics rather than running at constant pressure
and rhythm:

| Field | Drives | y semantics |
|---|---|---|
| `radius-stroke-curve` | brush `radius_logarithmic` (`stroke` input) | **log-space** offset added to base radius. `y = -2.3` ≈ ×0.1, `y = +0.69` ≈ ×2 |
| `opacity-stroke-curve` | brush `opaque` (`stroke` input) | linear offset added to base opaque |
| `hardness-stroke-curve` | brush `hardness` (`stroke` input) | linear offset added to base hardness |
| `dtime-stroke-curve` | per-vertex `dtime` | **multiplier** on the base `dtime`. `y = 3` slows the hand 3×, `y = 0.3` speeds it up |

Each curve is a piecewise-linear `[[t, y], ...]` where `t` is normalized
progress along the polyline (`t = 0` at the first vertex, `t = 1` at
the last). `t` values must be non-decreasing; at least two points are
required. Evaluation matches libmypaint's `InputMapping::eval`
(clamps below the first knot, extrapolates from the last segment).

When any of the **brush-side** curves (`radius` / `opacity` /
`hardness`) is set, `paint_lines` clones the brush per polyline and
auto-sets `stroke_duration_logarithmic = ln(line_length_px)` so the
brush's internal `stroke` input ramps from 0 → 1 over the full polyline
length on the rendered canvas. `dtime-stroke-curve` doesn't need a
clone — it scales the per-vertex `dtime` directly.

Example: ink-style taper (thin → fat → thin, faster in the middle):

```json
"roads_primary": {
  "op": "line", "features": "@roads_primary_f", "brush": "@glazing_brush",
  "color": "#3a2a18",
  "radius-stroke-curve":  [[0.0, -1.5], [0.15, 0.0], [0.85, 0.0], [1.0, -2.0]],
  "opacity-stroke-curve": [[0.0, -0.3], [0.1,  0.0], [0.9,  0.0], [1.0, -0.4]],
  "dtime-stroke-curve":   [[0.0, 3.0],  [0.15, 1.0], [0.85, 1.0], [1.0, 4.0]]
}
```

## Built-in nodes

`ezu_paint::nodes::default_registry()` returns a
[`NodeRegistry`](../ezu-graph) preloaded with:

**Raster utility** (`nodes::raster`)

| Op | Inputs → Output | Notes |
|---|---|---|
| `solid` | `() → Raster` | Constant-color fill |
| `circle` | `() → Raster` | Centered disk with optional edge falloff |
| `noise` | `() → Raster` | Procedural noise: `type` (`white`/`value`/`perlin`/`simplex`/`worley`), `scale-px`, fBm via `octaves`/`lacunarity`/`gain`, optional domain warp (`warp-amp`/`warp-freq`), `low-color`/`high-color` ramp, `opacity`, `anchor` (`world` default — seamless across tile borders) |
| `blur` | `Raster\|Sprite → same kind` | Gaussian (libblur); pass-through over `Raster`/`Sprite` — the output kind mirrors the input. Grows upstream pad by 3σ |
| `displace` | `Raster\|Sprite + Raster\|Sprite → mirrors main input` | Photoshop-style displacement map. `displacement` raster's R/G channels (0.5 = no offset) drive per-pixel offsets up to `amp-px`. Output kind mirrors the main `input`. Grows upstream pad by `amp-px`; `boundary` (`clamp`/`transparent`/`mirror`) handles edge sampling |
| `warp` | `Raster\|Sprite → same kind` | Domain warp via internal noise (same dial as `noise`: `type`, `scale-px`, `octaves`, `lacunarity`, `gain`, `seed`) plus `amp-px`. Pass-through over `Raster`/`Sprite`. `anchor: world` default → seamless across tile borders; grows upstream pad by `amp-px` |
| `blend` | `Raster\|Sprite base + over [+ mask] → mirrors base` | W3C blend modes (normal/multiply/screen/overlay/darken/lighten/color-dodge/color-burn/hard-light/soft-light/difference/exclusion/hue/saturation/color/luminosity), `composite` operator (`over` default / `destination-out` for brush-eraser), `clip` (source-atop, PS clipping mask), optional alpha `mask`, `opacity`. All three inputs accept `Raster` or `Sprite`; output kind mirrors `base` |
| `brightness-contrast` | `Raster\|Sprite → same kind` | Linear brightness shift + contrast slope around mid-gray; pass-through over `Raster`/`Sprite` |
| `levels` | `Raster\|Sprite → same kind` | Photoshop-style levels: remap `[in-black, in-white]` through `gamma` onto `[out-black, out-white]`; generalises `brightness-contrast` with a midtone curve |
| `erode` / `dilate` | `Raster\|Sprite → same kind` | Per-channel morphological min / max over a square kernel of `radius-px`. Classic mask cleanup after `color-to-alpha`. Grows upstream pad by `radius-px` |
| `edge-detect` | `Raster\|Sprite → same kind` | Sobel gradient magnitude per channel, scaled by `strength` and clamped. Grows upstream pad by 1 |
| `hsl` | `Raster\|Sprite → same kind` | Hue rotation (degrees) + saturation/lightness shift in `[-1, 1]`; pass-through over `Raster`/`Sprite` |
| `invert` | `Raster\|Sprite → same kind` | Negate RGB (alpha preserved); pass-through over `Raster`/`Sprite` |
| `color-to-alpha` | `Raster\|Sprite → same kind` | Chroma-key: pixels near `color` (Chebyshev distance) become transparent with `threshold`/`softness` ramp; pass-through over `Raster`/`Sprite` |
| `posterize` | `Raster\|Sprite → same kind` | Quantise each RGB channel into `steps` evenly-spaced levels (non-premultiplied sRGB). Alpha preserved |
| `channel-shuffle` | `Raster\|Sprite → same kind` | Rearrange RGBA channels: each output `r`/`g`/`b`/`a` names which input channel (or constant `0`/`1`) feeds it. Operates in non-premultiplied sRGB |
| `sharpen` | `Raster\|Sprite → same kind` | 4-neighbour Laplacian sharpen with strength `amount`. Grows upstream pad by 1 |
| `gradient-linear` | `() → Raster` | Linear gradient between two points. `start`/`end` as `[x, y]` fractions, `stops: [[t, "#hex"], …]`, optional `anchor: "tile" \| "world"` |
| `gradient-radial` | `() → Raster` | Radial / elliptical gradient. `center`, `radius`, optional `aspect` |
| `gradient-conic` | `() → Raster` | Sweep gradient around `center` starting at `start-angle` (degrees) |
| `gradient-diamond` | `() → Raster` | Manhattan-distance gradient. `center`, `radius` |
| `hillshade` | `ScalarField → Raster` | Horn-method analytical hillshade. `azimuth-deg` / `altitude-deg` light angle, `z-factor` / `exaggeration`, optional ESRI `multidirectional`. `mode: shade` (grayscale) or `mode: relief` (transparent black for multiply-blend over a base map). Geographically accurate only when the input's `geo_scale` is populated (DEM source); otherwise produces pixel-space gradients (fine for stylization) |
| `slope` | `ScalarField → Raster` | Per-pixel slope angle as grayscale, normalised to `0..1` against `max-deg`; optional `invert`. Same `geo_scale` caveat as `hillshade` |
| `color-ramp` | `ScalarField → Raster` | Map scalar values to colour via a `stops: [{value, color}]` table; linear interp, end colours clamp out-of-range. Canonical use is hypsometric tinting over an elevation `ScalarField` (`stops[i].value` = metres) but works on any scalar field |
| `map-range` | `ScalarField → ScalarField` | Linearly remap from `[in-min, in-max]` to `[out-min, out-max]` with optional `clamp`. Normalise a DEM or distance field into `[0, 1]` before `color-ramp` |
| `threshold` | `ScalarField → ScalarField` | Binarise against `value`: emit `low` for samples ≤ `value`, `high` otherwise; `softness` gives a linear ramp instead of a hard step |

**Sources** (`nodes::source`)

| Op | Inputs → Output | Notes |
|---|---|---|
| `features` | `() → Features` | Samples a host-bound layer (`tile.<layer>` for per-tile MVT/GeoJSON) via `AssetLoader` |
| `dem` | `() → ScalarField` | Samples a host-bound DEM mosaic (`tile.<source>` matching a `sources` entry). The host fetches + decodes raster-DEM tiles (terrarium / mapbox-rgb) and binds the stitched scalar field (with `geo_scale` populated) per render |
| `literal-geometry` | `() → Features` | Inline points / lines / polygons from style fields |
| `tile-bounds` | `() → Features` | Polygon covering the current tile |
| `point-grid` | `() → Features` | Regular grid of points across the tile |

**Feature paint** (`nodes::paint`)

| Op | Inputs → Output | Notes |
|---|---|---|
| `fill-solid` | `Features → Raster` | wraps `paint_polygons` |
| `fill-dabs` | `Features → Raster` | wraps `paint_polygons_dabs` |
| `line` | `Features + Brush → Raster` | wraps `paint_lines` |
| `brush-file` | `() → Brush` | Resolved by the host's `AssetLoader` |

**Geometry ops** (`nodes::geometry`) — turf.js-flavored `Features → Features` transforms

| Op | Inputs → Output | Notes |
|---|---|---|
| `centroid` | `Features → Features` | Polygon / line centroids as points |
| `boundary` | `Features → Features` | Polygon rings as lines |
| `simplify` | `Features → Features` | Douglas–Peucker |
| `convex-hull` | `Features → Features` | Convex hull over all input vertices |
| `buffer` | `Features → Features` | Offset / Minkowski-style buffer |
| `hatch` | `Features → Features` | Hatch-line fill of polygons |
| `voronoi` | `Features → Features` | Voronoi diagram of input points → edge polylines (2-point each). Polygons/lines ignored — pipe `centroid` upstream to derive seeds |
| `voronoi-fracture` | `(Features, Features) → Features` | Fracture each polygon in `features` into Voronoi sub-cells seeded by `seeds`' points; cells clipped to the source polygon |
| `medial-axis` | `Features → Features` | Approximate medial axis (skeleton) of each input polygon as polylines. `densify-px` controls boundary sampling, `min-branch-px` prunes short branches. Useful for river / lake centrelines |

Each factory implements `NodeFactory::schema()` so editors picking up
the registry-derived JSON Schema get per-op autocomplete. Adding a new
op means dropping a file under the right category and ending it with
`ezu_graph::submit_node!(MyFactory);` — no central list to edit.

## Canvas

```rust
pub struct Canvas { /* … */ }
impl Canvas {
    pub fn new_padded(tile_w: u32, tile_h: u32, pad: u32) -> Self;
    pub fn pixmap(&self) -> &tiny_skia::Pixmap;
    pub fn pixmap_mut(&mut self) -> &mut tiny_skia::Pixmap;
    pub fn into_pixmap(self) -> tiny_skia::Pixmap;          // zero-copy handoff
    // accessors for width / height / tile_width / tile_height / pad
}
```

The canvas paints into a **padded** buffer (`tile + 2 * pad`) so blurs
extend cleanly through the tile edge and MVT buffer geometry that
overflows `[0, extent]` lands inside the buffer. Internal node impls
construct a Canvas, paint into it, then `into_pixmap().take()` to hand
the pixel `Vec<u8>` to the graph layer without a memcpy.

## Host glue

```rust
use ezu_paint::host::{BrushBankLoader, TileLoader, raster_to_png, raster_to_webp, raster_to_rgba8};

let mut assets = BrushBankLoader::new().with_dir("assets/brushes".into());
assets.insert("watercolor_glazing", hokusai::myb::from_str(&myb_json)?);

// Per render, overlay tile-scoped feature layers on top of the base
// loader. `bind_mvt` registers every layer under `tile.<layer-name>`.
let mut tile_loader = TileLoader::new(&assets, tile_id);
tile_loader.bind_mvt(ezu_features::mvt::decode(&bytes)?);

let ev = Evaluator::new(&graph, &cache, &tile_loader);
let raster = ev.render(tile_id, canvas, &params, seed)?;
let png  = raster_to_png(&raster, tile_size, pad)?;       // cropped + PNG
let webp = raster_to_webp(&raster, tile_size, pad)?;      // cropped + lossless WebP
let rgba = raster_to_rgba8(&raster, tile_size, pad);      // cropped, straight RGBA
```

`BrushBankLoader` implements `AssetLoader` for document-scoped images
and brushes (in-memory + disk fallback). `TileLoader` is a per-render
overlay that adds tile-scoped feature bindings on top of any base
loader. Both compose freely with custom `AssetLoader` impls.

### `tile.<layer>` convention

Anything the `features` node refers to as `tile.<name>` is expected to
be bound by the host once per tile. `TileLoader::bind_mvt(decoded)`
walks every layer in a decoded MVT and registers each one under
`tile.<layer-name>`; a custom binding (GeoJSON, in-memory synthesized
data, …) goes through `bind_features("tile.<name>", layer)`. Names
without the `tile.` prefix flow through to the base loader unchanged,
which is where document-scoped image / brush assets live.

`raster_to_png` / `raster_to_webp` / `raster_to_rgba8` all crop the
padded buffer down to the central tile region before encoding /
demultiplying. WebP uses the pure-Rust `image-webp` codec (lossless
only) — no native deps. A `pixmap_to_webp(&tiny_skia::Pixmap)` helper
covers non-tile-sized outputs (e.g. CLI bbox mosaics).

### DEM sources (feature `http`)

`host::dem` ports the same `sources`-driven pattern to raster-DEM
tiles. `build_dem_sources(doc)` walks the style's `sources` block,
building one fetcher (terrarium or mapbox-rgb, PNG or WebP) per
declared source; `bind_dem_sources(&mut tile_loader, &registry, tile,
canvas)` fetches the 3×3 neighbourhood (date-line-wrapping in X,
edge-clamping in Y), bilinear-resamples it onto the padded canvas,
and binds the resulting `ScalarField` under `tile.<source-name>` so
the style's `dem` node picks it up. Requests beyond the source's
`max-zoom` upsample from the appropriate ancestor tile. Decoded tiles
are cached unboundedly per source — well-suited to single-tile and
modest-pyramid renders; swap in an LRU bound if working sets ever
outgrow memory.

## Features

- `parallel` — pull-through to `ezu-graph/parallel` (Rayon within-tile
  evaluation). No effect on the paint primitives themselves; the hot
  loops inside `hokusai` are still single-threaded.
- `http` — enable `host::prefetch_doc_assets` (walks a parsed
  `Document`'s `assets` block, fetches every `http(s)://` `src` with
  `reqwest`, and stages the decoded brush / image into a
  `BrushBankLoader`) and the `host::dem` module (raster-DEM tile
  fetcher + 3×3 stitch + overzoom upsampling that feeds the
  `ScalarField` port). Off by default so `wasm32` keeps its dep graph
  minimal (the JS host fetches assets directly there).

## License

MIT or Apache-2.0, at your option.
