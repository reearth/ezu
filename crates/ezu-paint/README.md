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
3. **`host` module** — host-side glue: `AssetLoader` impl, conversions
   from `RasterBuf` to PNG / straight RGBA.

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
| `blur` | `Raster → Raster` | Gaussian (libblur); grows upstream pad |
| `blend` | `Raster base + Raster over [+ Raster mask] → Raster` | W3C blend modes (normal/multiply/screen/overlay/darken/lighten/color-dodge/color-burn/hard-light/soft-light/difference/exclusion/hue/saturation/color/luminosity), optional `clip` (source-atop, PS clipping mask), optional alpha `mask`, `opacity` |

**Feature sources** (`nodes::source`)

| Op | Inputs → Output | Notes |
|---|---|---|
| `mvt-source` | `() → Features` | Pulls a layer out of `EvalCtx::tile_data` |
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
use ezu_paint::host::{BrushBankLoader, raster_to_png, raster_to_rgba8};

let mut assets = BrushBankLoader::new().with_dir("assets/brushes".into());
assets.insert("watercolor_glazing", hokusai::myb::from_str(&myb_json)?);

// after Evaluator::render_with_tile_data returns a RasterBuf:
let png = raster_to_png(&raster, tile_size, pad)?;       // cropped + PNG
let rgba = raster_to_rgba8(&raster, tile_size, pad);     // cropped, straight RGBA
```

`BrushBankLoader` implements `AssetLoader`. It checks an in-memory
`HashMap<String, Arc<Brush>>` first, then falls back to reading
`<dir>/<name>.myb` from disk — works the same way for the `tokyo`
example, `ezu-server`, and unit tests.

`raster_to_png` / `raster_to_rgba8` crop the padded buffer down to the
central tile region before encoding / demultiplying.

## Features

- `parallel` — pull-through to `ezu-graph/parallel` (Rayon within-tile
  evaluation). No effect on the paint primitives themselves; the hot
  loops inside `hokusai` are still single-threaded.

## License

MIT or Apache-2.0, at your option.
