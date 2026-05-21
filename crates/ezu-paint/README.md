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
   op, plus `default_registry()` that wires them all up.
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

## Built-in nodes

`ezu_paint::nodes::default_registry()` returns a
[`NodeRegistry`](../ezu-graph) preloaded with:

| Op | Inputs → Output | Notes |
|---|---|---|
| `solid` | `() → Raster` | Constant-color fill |
| `mask-solid` | `() → Mask` | Constant-value mask |
| `mask-circle` | `() → Mask` | Centered disk; useful for tests |
| `mask-blur` | `Mask → Mask` | Separable gaussian; grows upstream pad |
| `fill-with-mask` | `Mask → Raster` | Tint a mask with a color |
| `blend` | `Raster + Raster → Raster` | Premul source-over with opacity |
| `mvt-source` | `() → Features` | Pulls a layer out of `EvalCtx::tile_data` |
| `fill-solid` | `Features → Raster` | wraps `paint_polygons` |
| `fill-dabs` | `Features → Raster` | wraps `paint_polygons_dabs` |
| `line` | `Features + Brush → Raster` | wraps `paint_lines` |
| `brush-file` | `() → Brush` | Resolved by the host's `AssetLoader` |

Each factory implements `NodeFactory::schema()` so editors picking up
the registry-derived JSON Schema get per-op autocomplete.

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
