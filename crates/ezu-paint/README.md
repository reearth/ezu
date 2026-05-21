# ezu-paint

Rendering core of the [`ezu`](../../README.md) workspace.

Bridges `ezu-mvt`'s decoded features and `hokusai`'s brush engine: turns
polygons / lines into hokusai dabs and strokes, applies optional gaussian
blur, and emits a PNG or straight RGBA buffer.

## Paint primitives

| Function | Type | What it does |
|---|---|---|
| [`paint_polygons`] | `fill-solid` | `tiny-skia` solid fill + optional outline + `libblur` gaussian blur |
| [`paint_polygons_dabs`] | `fill-dabs` | `hokusai` scatter-dab fill with **world-deterministic** position/size/opacity jitter — same world coord → same dab regardless of tile |
| [`paint_lines`] | `line` | `hokusai::Brush::stroke_to` per polyline vertex with world-seeded pressure jitter |

For `fill-dabs` the polygon is rasterized to a binary mask, then a regular
grid of candidate positions is iterated; no brush trajectory is constructed,
which is what keeps fills seamless across tile boundaries.

## Canvas

```rust
pub struct Canvas { /* … */ }
impl Canvas {
    pub fn new_padded(tile_w: u32, tile_h: u32, pad: u32) -> Self;
    // accessors for tile_width / tile_height / pad
}

pub fn canvas_from_style(style: &ezu_style::Style) -> Canvas;
pub fn canvas_from_style_sized(style: &Style, tile_size: u32, pad: u32) -> Canvas;
pub fn to_rgba8(canvas: &Canvas) -> Vec<u8>;     // straight RGBA, cropped to tile
pub fn encode_png(canvas: &Canvas) -> Result<Vec<u8>, PaintError>;
```

The canvas paints into a **padded** buffer (`tile + 2 * pad`) so blurs
extend cleanly through the tile edge and MVT buffer geometry that
overflows `[0, extent]` lands inside the buffer. `to_rgba8` / `encode_png`
crop back to the actual tile.

## Render dispatcher

```rust
pub fn render_style(
    canvas: &mut Canvas,
    style: &ezu_style::Style,
    decoded: &ezu_mvt::DecodedTile,
    tile: ezu_core::TileId,
    resolve_brush: &dyn Fn(&str) -> Option<&hokusai::Brush>,
) -> Result<(), RenderError>;
```

`resolve_brush` lets callers wire up a custom brush bank — typically a
`HashMap<String, Brush>` populated from `.myb` files. The `@name` prefix
used in styles is stripped before the lookup.

See the main [README](../../README.md) for the full project overview.

## License

MIT or Apache-2.0, at your option.
