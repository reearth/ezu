# Changelog

Every published crate and the npm package share one version and are released as
a set. Each version's section below is what its
[GitHub release](https://github.com/reearth/ezu/releases) says; releases before
0.10.0 predate this file and have only their commit log.

ezu is pre-1.0, so a minor bump may carry a breaking change. What the numbers
mean is spelled out in
[versioning](https://reearth.github.io/ezu/reference/changelog/).

## Unreleased

### Added

- Each release ships a static musl build of the CLI, for Alpine and any Linux
  without glibc: `ezu-<tag>-x86_64-unknown-linux-musl.tar.gz` and
  `ezu-<tag>-aarch64-unknown-linux-musl.tar.gz`. They allocate through
  mimalloc, since musl's own allocator made a brush-heavy page take about
  twice as long as the glibc build; with it they render as fast, and to the
  same bytes.
- The Rust renderer, the Go package and the npm package render print pages
  the way `ezu frame` does. `compute_frame` (`ComputeFrame`, `computeFrame`)
  works out the zoom, tile size and covered tiles for a scale, dpi, page size
  and centre, with the CLI's own arithmetic, and `render_frame`
  (`RenderFrame`, `renderFrame`) renders those tiles, asking the host for
  the source set each one renders from, and crops them into one RGBA page
  that matches the CLI's pixel for pixel; from JS the callback may return a
  promise, so it can fetch. A host that keeps the tiles it loads puts each
  into the set of every page tile that reads it and decodes it once. What a
  set needs bound into the renderer, such as glyphs, goes through a
  `FrameBinder` that works only for its own tile. While the frame renders
  nothing else can change the renderer: in Rust the borrow checker forbids
  it, and in Go and JS such a call fails with `ErrFrameInProgress`
  (`FrameInProgress`). `set_tile_size` (`SetTileSize`, `setTileSize`) and
  its clearing counterpart render at another tile size without editing the
  style, for a host that renders a frame's tiles itself.
- `ezu::renderer` is the embedding renderer the JS and Go packages wrap, now
  part of the umbrella crate (the `renderer` feature, on by default), so a
  Rust host that does its own fetching can bind and render — a tile or a
  print page — with the same calls.
- The renderer can render a tile from data handed to the call instead of
  from what is bound to it, in Rust, Go and JS. `load_source` (`LoadSource`,
  `loadSource`) decodes a vector, DEM, imagery or GeoJSON payload once into a
  `SourceData`; a `SourceSet` holds the payloads one render reads, each at
  its neighbour offset, and clones without copying them (`clone`, `Clone`,
  `clone()`); and `render_tile_with` (`RenderTileWith`,
  `renderTileWith`) renders from a set and nothing else that is
  tile-scoped. A tile loaded once can sit in the set of every tile it
  borders, so a block of tiles decodes each tile once instead of once per
  render it appears in, and in Rust renders on several threads can share
  one renderer. `needed_codepoints_in` and `needed_glyph_ranges_in` answer
  for a set, and `trim_glyphs` applies a glyph budget, which a render from a
  set leaves to the host. In Go a `SourceData` belongs to the renderer that
  loaded it and is freed with `Release`; in JS with `free()`. `bind_source`
  and `render_tile` work as before, and a vector tile bound with
  `bind_source` is now decoded once rather than at both the bind and the
  render.
- A render can hand back what a features node held next to the image, in
  Rust, Go and JS: `render_tile_with_features` (`RenderTileWithFeatures`,
  `renderTileWithFeatures`) renders as `render_tile_with` does and returns,
  for each node id asked for, the node's features from that same evaluation
  as a GeoJSON FeatureCollection in tile units, so a host can list the
  labels on a tile and what placement did to them without rendering twice.
  `render_frame_with_features` (`RenderFrameWithFeatures`,
  `renderFrameWithFeatures`) does the same for a print page: each node's
  features gathered over the page's tiles, copies that neighbouring tiles
  both saw collapsed by an optional key, in lon/lat or page pixels, written
  as the same bytes `ezu frame --features` writes. A host that renders a
  page's tiles itself gathers the same pages through `frame_features`
  (`FrameFeatures`, `frameFeatures`): a collector that takes each tile, in
  any order, and assembles the pages at the end.
- The renderer answers what building the style's graph warned about, in
  Rust, Go and JS: `warnings()` (`Warnings`, `warnings`), for a host that
  reports them or refuses a style with any, as `ezu check --strict` does. A
  Rust host can also read the built graph itself with `graph()` rather than
  building the style a second time. The wasm module's ABI version is now 7.
- The renderer's cache size can be set, in Rust, Go and JS:
  `set_cache_budget` (`SetCacheBudget` and `ClearCacheBudget`,
  `setCacheBudget` and `clearCacheBudget`) lets the render cache hold more
  or less than its default 8 MiB of pixels, for a batch of neighbouring
  tiles that reuse each other's work or for a tight memory limit, and keeps
  the size across a new style.
- A native Rust host can let the renderer read a style's assets itself.
  `resolve_assets`, with the `http` feature, reads or fetches every brush,
  image, sprite and font the style declares, `file:` paths against a base
  directory, and reads each glyph range the first time a label needs it,
  as `ezu` does, so such a host binds only its tile data. It is Rust-only:
  the JS and Go packages run where nothing can do I/O from inside a render,
  so there every source is bound as before.
- A Rust host can watch every node of a render: `render_tile_observed`
  renders as `render_tile_with` does and shows each node's value, and how
  long it took, to a `NodeObserver`. It is the hook `ezu graph --tile` draws
  its pictures from, and Rust-only, since the values are the evaluator's
  in-memory types.
- `text` and `text-labels` take `anchor-expr`, a MapLibre expression that
  names a point label's anchor per feature, so one layer can put some labels
  left of their points and others right of them, or switch sides with zoom.
  It stands in for `anchor`, with `offset-em` shifting the block as it would
  for that anchor; a result that names no anchor falls back to `anchor`. It
  is ignored, with a warning, alongside `anchor-offsets` or `anchor-variants`,
  and on line placement.
- `segment-to` draws to polygons too. The line runs from the longitude and
  latitude the feature names towards the polygon's centroid and stops where it
  first meets the polygon's edge, so a leader to a label box from `label-boxes`
  ends at the box instead of running under the text; there is no line when the
  position lies inside the polygon. `gap-px` takes that many pixels off the
  feature end of every line, at a polygon's edge or a point, to leave some air
  between the leader and what it reaches. Polygons were ignored before, and
  the gap defaults to 0, so existing styles draw as they did.
- `label-boxes` takes `filter-expr`, a MapLibre expression tested against each
  box with every property it carries, `part` included. Only the boxes for which
  it is true are emitted, so a recipe can keep the labels that moved off their
  first position (`[">", ["coalesce", ["get", "variant-index"], 0], 0]`) to
  draw leader lines to, or the ones that lost with `include: "all"`.
- `text` and `text-labels` take `dedupe-key-expr` and `dedupe-distance-px`,
  to show a repeated name once. Once a point label has placed, later labels in
  the same layer whose key is equal and whose anchor is within the distance are
  dropped, so a name that several neighbouring features share shows once. Only
  placed labels count, so a label that loses its collision never hides its
  duplicates, and the decision is the same on every tile. `label-boxes` with
  `include: "all"` reports a dropped label with `drop-reason: "dedupe"`.
- `label-boxes` says what placement did with each label. A placed label's
  boxes carry `variant-index`, where the position it took sits in the list it
  was tried from (`anchor-offsets`, or the anchors of `anchor-variants`; 0 for
  the first, for a lone anchor and for a line label), and `overlapped`, true
  when `overlap` showed it over a label placed before it. A label that lost,
  with `include: all`, carries `drop-reason: "collision"`. A recipe can now
  pick out the labels that moved off their first anchor, to draw a leader
  line to their feature, say, or the ones left overlapping.
- `text` and `text-labels` take `anchor-offsets`, a list of positions a point
  label tries in order, each an `anchor` with its own shift: `offset-em`
  [x, y] as written, or `radial-em`, pushed away from the point in the
  anchor's direction. `anchor-variants` gave every anchor the same distance;
  now each position has its own, and the same anchor can appear more than
  once, so a crowded label can try a spot a little further off before moving
  to another side. With `anchor-offsets` set, `anchor`, `offset-em`,
  `anchor-variants` and `radial-offset` are ignored for point placement, and
  the build warns which ones it ignored.
- `text` and `text-labels` take `overlap`: `never` (the default), `always`,
  or `fallback`. `always` is what `allow-overlap: true` did: the label shows
  at its first position without trying the others. `fallback` tries every
  position first, as a label normally does, and only when all of them are
  blocked shows the label at its first position anyway, overlapping, where
  `never` would drop it. A label that must appear can now still move out of
  the way when there is room; `label-boxes` marks the ones that could not as
  `overlapped`.
- `ezu translate` converts MapLibre's `text-variable-anchor-offset`, which
  pairs each anchor with its own offset, to `anchor-offsets`. It takes
  precedence over `text-anchor`, `text-offset`, `text-variable-anchor` and
  `text-radial-offset`, as in MapLibre. Before, it was dropped and the label
  fell back to those.
- `ezu frame` renders a map at a print scale and paper size, cropped exactly
  to the page: `--center 139.70,35.66 --scale 1000 --dpi 200 --size-mm
  245x174` (or `--size-px`). It picks the zoom whose tile size comes nearest
  the style's `tile-size`, renders at the tile size that meets the scale
  (`--zoom` fixes the zoom instead), and logs the scale it reached. Each tile
  is copied straight into the page as it finishes, so the whole tile mosaic is
  never held in memory, as it is with `ezu bbox`. `--dpi` only sets the
  scale; the style's pixel sizes are not enlarged. The geometry is
  `ezu::core::frame` for hosts that render pages themselves.
- `ezu frame --features NODE=PATH` writes a features node, gathered over
  every tile of the page, as a GeoJSON FeatureCollection beside the image:
  with `label-boxes`, a list of every label and what placement did to it, to
  report the dropped ones somewhere other than the map, say. `--features-key
  NODE=PROP[,PROP…]` lists features whose properties are equal once, where
  neighbouring tiles each see them, and sorts by those properties; for
  `label-boxes` that is `label-id,part`. `--features-coords` writes WGS84
  lon/lat (the default) or pixels of the output image. Features wholly
  outside the page are left out, and the file is byte-identical across runs
  of the same input.
- `ezu frame --pages PATH` renders every page a JSON file lists in one run.
  Each page has the fields of the single-page flags (`center`, `scale`,
  `dpi`, `size-mm` or `size-px`, `zoom`, `out`, `format`, `features-coords`,
  and `features` as `{NODE: {"out": PATH, "key": [PROP, …]}}`), and a
  `defaults` object supplies any but `out`. The style is read and its sources
  opened once, and vector tiles fetched for one page are kept in memory for
  the next (256 MB, or `EZU_TILE_CACHE_MB`), so a job of neighbouring pages
  fetches its shared background tiles once. Each page is byte-identical to a
  single-page run of it. The file is checked before anything renders, and a
  mistake names the page.
- A `dem` source takes `"encoding": "custom"` with `red-factor`,
  `green-factor`, `blue-factor` and `base-shift`, and decodes each pixel as
  `R*red-factor + G*green-factor + B*blue-factor - base-shift`, as MapLibre's
  `custom` encoding does, for elevation tiles that use neither Terrarium nor
  Terrain-RGB. The factors default to 1 and the shift to 0; giving one with
  another encoding fails to load, since it would do nothing.
- `raster` and `dem` nodes take `min-zoom` and `max-zoom`, as `features`
  and `text` do. Outside that range a `raster` node draws transparent
  pixels and a `dem` node a zero field, which is what each yields when its
  source has no tile.
- `dem` and `raster` sources take `min-zoom`, as `mvt` and `pmtiles` do.
  Below it the CLI fetches nothing for the source, the renderer leaves
  whatever is bound for it unread, and `on-missing: error` does not fire;
  `on-missing: upsample` walks up no further than it.
- `mvt` and `pmtiles` sources take `on-missing`, as `dem` and `raster` do:
  `empty` draws no features for a missing tile, `upsample` reprojects the
  nearest ancestor that exists (no higher than `min-zoom`), and `error`
  fails the render — in the CLI, `ezu serve` answers 404, and the Rust, Go
  and JS renderers fail a tile with nothing bound for the source. Left out,
  nothing changes: the CLI still looks `--overzoom-levels` parents up.
- `line-stamp` takes `width-expr` and `opacity-expr`, evaluated per feature
  group as on `stroke`, and a pattern named per feature: `sprite` plus
  `name-expr` in place of the `image` input, as on `stamp`.
- `line` takes `color-expr`, `radius-expr` and `opacity-expr`, evaluated per
  feature group. The groups share one brush surface and the pressure jitter
  stays seeded by world position, so a style that resolves every group to
  the constants draws exactly what the constants draw.
- `fill-dabs` takes `color-expr` and `opacity-expr`, evaluated per feature
  group. The dabs land and jitter where they did, and each takes the paint
  of the group whose polygon covers its centre.
- `circles` takes `stroke-opacity` (MapLibre `circle-stroke-opacity`), the
  ring's own opacity in place of `opacity`, and `blur` (`circle-blur`), a
  fade to the edge over that fraction of the radius, each with a `*-expr`
  form. Left out, the ring takes `opacity` and the edge is crisp, as before.
- `stamp` takes `anchor` (the nine names `icon-anchor` takes on `text`),
  `anchor-expr` and `offset` in px, as MapLibre's `icon-anchor` and
  `icon-offset`: the sprite sits on the point by that part, shifted by the
  offset, and scales and turns about the point. Left out, it stays centred.
- `fill-solid` takes `edge-expr`, the outline colour per feature group. Its
  alpha is the outline's opacity, as the constant `edge`'s is.
- `text` and `text-labels` take `justify-expr`, `offset-em-expr`,
  `max-width-em-expr`, `letter-spacing-em-expr`, `line-height-expr` and
  `transform-expr`, MapLibre expressions evaluated per feature group with the
  tile's zoom in place of their constants, so a zoom curve on
  `text-max-width` or a `match` on `text-justify` lays each label out the way
  MapLibre does. Labels with the same text but different results lay out
  separately. `offset-em-expr` applies wherever `offset-em` does, and like it
  is ignored, with a warning, when `anchor-offsets` sets a point label's
  positions.
- `text` and `text-labels` take `rotate-deg` and `rotate-deg-expr`, MapLibre's
  `text-rotate`: a point label turns clockwise about its anchor, its offset,
  background box and strikethrough with it. It collides by the axis-aligned
  bounds of its turned box, as in MapLibre, and `label-boxes` reports its
  `text` and `background` as turned quads. Line placement ignores it with a
  warning.
- `ezu translate` carries expression forms of `text-offset`,
  `text-max-width`, `text-letter-spacing`, `text-justify`,
  `text-line-height` and `text-transform`, zoom curves included, to the
  `text` node's matching `*-expr` fields, and `text-rotate` on a point-placed
  layer to `rotate-deg` / `rotate-deg-expr`. They used to warn and fall back
  to the default. `text-rotate` on a line-placed layer still warns, as does
  an expression `text-offset` alongside `text-variable-anchor`.
- `blend`'s `composite` takes all twelve Porter-Duff operators by their W3C
  Compositing names: `clear`, `copy`, `destination`, `source-over`,
  `destination-over`, `source-in`, `destination-in`, `source-out`,
  `destination-out`, `source-atop`, `destination-atop` and `xor`. The blend
  mode mixes the colours where both inputs are present, and the operator
  decides how much of each survives, as the spec combines them. `over`,
  `dest-out` and `erase` still work, and `source` names `copy`.
- `math` and `field-math` take `exp`, `log` (natural), `log2`, `log10`,
  `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `sign`, `fract`,
  `step` and `smoothstep`. Angles are in radians, and the results are the
  same bytes on every host.
- `flow-smear` takes `sides: "backward"`, which smears only against the
  field, so a speck trails upstream where `forward` trails it downstream.
- `channel-shuffle` can feed a channel from the input's `luminance`, the
  same Rec. 601 luma `pick-channel` reads.
- `displace` and `warp` take `boundary: "wrap"` (or `"repeat"`): a read past
  one edge of the image comes back in at the opposite edge, as a sprite that
  repeats edge to edge needs. A scalar field still clamps at its edge.
- `color-ramp` takes `interpolation: "step"`, which gives each value the
  colour of the stop at or below it, as MapLibre's `step` does, so the ramp
  paints flat bands. `linear` stays the default.
- `graticule` takes `interval-x-deg` (between meridians) and
  `interval-y-deg` (between parallels), as `point-grid` takes `spacing-x`
  and `spacing-y`. `interval-deg` fills whichever is unset, and an axis
  with neither still follows the zoom.
- A tile's features can be written in longitude and latitude or in the
  rendered tile's pixels, as well as in tile units, in Rust, Go and JS:
  `NodeFeatures::to_geojson_in` with a `TileFeatureCoords`, the
  `FeaturesIn` option to `RenderTileWithFeatures`, and
  `renderTileWithFeatures(z, x, y, set, { nodes, coords })`. The positions
  are written as a print page's are, so a tile's longitudes are the ones a
  page holding it writes; tile units stay the default, byte for byte. The
  wasm module's ABI version is now 8.
- The JS and Go packages answer the same diagnostics and setters. JS gains
  `clearGlyphBudget()`, as Go's `ClearGlyphBudget` (`0xFFFFFFFF` to
  `setGlyphBudget` still lifts the cap too), and `opCount()` and
  `opNames()`, as Go's `OpCount` and `OpNames`. Both gain the tile size
  renders use, which `set_tile_size` may have changed from the style's:
  `renderTileSize` in JS and `RenderTileSize` in Go, as Rust's
  `render_tile_size`.
- `ezu tile`, `ezu bbox`, `ezu tiles` and `ezu serve` take `--tile-size`,
  which renders every tile at that size instead of the style's
  `tile-size`, as the embedding renderer's `tileSize` option does, and all
  of them and `ezu frame` take `--pad`, which replaces the margin rendered
  outside each tile. The style's pixel sizes stay as they are, so a larger
  tile draws finer. `frame` takes no `--tile-size`: its tile size is what
  meets the scale.
- Tiles render at a device pixel ratio: `pixel_ratio` in Rust's
  `RenderOptions`, `PixelRatio` in Go's `Render`, `pixelRatio` in the JS
  render options, and `--pixel-ratio` on `ezu tile`, `bbox`, `tiles` and
  `serve`, from 1 to 4 with fractions allowed. At 2 a tile is the same map
  at twice the density, as MapLibre's retina tiles are: twice the tile
  size and margin, with every line width, text and icon size, halo, blur,
  dash, offset, spacing and noise scale the style gives in pixels doubled,
  and the same labels placed in the same spots. `ezu serve` answers
  `/tiles/{z}/{x}/{y}@2x.png` (`@1x` to `@4x`, PNG or WebP) with the tile
  at that pixel ratio, the URL a MapLibre client asks a hi-DPI tile by.
  A print frame takes no ratio: its dpi is its density.
- `ezu bbox` takes `--concurrency`, as `frame` and `tiles` do, for how many
  of the mosaic's tiles render at once. It still defaults to the core count.
- The CLI commands that write PNG take `--png-compression fast|default|best`,
  the effort the embedding packages' `png.compression` names: `tile`,
  `bbox`, `frame`, `tiles`, `serve` and `legend` default to `default`, and
  `graph`, whose node pictures were always encoded fast, to `fast`. The
  pixels are the same at every level. `tile`, `bbox`, `frame` and `tiles`
  also take `--format rgba` (or an `.rgba` extension, and `"format": "rgba"`
  on a frame page): straight 8-bit RGBA, row-major, with no header, as the
  packages' `rgba` format returns it.
- `ezu bbox` takes `--features`, `--features-key` and `--features-coords`,
  and writes a mosaic's features as `frame` writes a page's, the mosaic
  being a page that is exactly its tiles. `ezu tile` takes `--features` and
  `--features-coords` (`tile`, the default, `lonlat` or `px`) and writes one
  tile's collection as the embedding packages' `renderTileWithFeatures`
  returns it.
- Params can be strings: `{ "type": "string", "default": "multiply",
  "enum": ["multiply", "screen"] }`. `enum` is optional; when it is given the
  default must be one of its values, and an override from `--param`, the
  tile server's query string, or the Rust, Go and JS `params` is refused
  outside it, as a number outside `min`/`max` is. `params_schema()`
  (`ParamsSchema`, `paramsSchema`) and `/style/params` carry the `enum`,
  and the live editor's params panel shows such a param as a menu, or a
  text field without one.
- Every field that takes one of a fixed set of names — `blend`'s `mode`,
  `stroke`'s `cap`, `noise`'s `type`, a `boundary`, an `anchor` — takes a
  `$param` of type `string`, which most of them follow per render. Among
  them are all of `text`'s and `text-labels`' — `placement`, `anchor`,
  `justify`, `transform`, `overlap`, `polygon-anchor`, `icon-anchor` and
  `icon-text-fit` — so one style can switch its labels between point and
  line placement per render, the point-only fields (icon, box, rotation,
  dedupe, `anchor-expr`) applying whenever it places points. A field's
  `*-expr` still takes its place for the features it names. The build
  fails when the param's default, or any value its `enum` allows, is not a
  name the field accepts. The few fields the graph needs before rendering
  (a generator's `kind`, `math`'s `fn`, `mosaic`'s `mode`, `features`'
  `stitch`) take the param's default, with the build-time warning other
  such fields give. The schema marks each of these fields as taking a
  `$param`.
- `switch` picks among any number of branches, in two new forms beside
  `a` / `b`. `branches` maps names to nodes and `select` names one, a
  literal or a `string` param, with an optional `default`, as MapLibre's
  `match`; the build fails if the param can name a branch that is not
  there and there is no `default`. `cases` is a list of
  `{ "when": …, "then": "@node" }` tried in order, with a required
  `default`, as MapLibre's `case`; a `when` is a bool literal, `$param`
  or `@node`. A two-way `select` may also be a `string` param naming `a`
  or `b`.
- Every generator offers the same kinds. `gradient-linear`,
  `gradient-radial`, `gradient-conic`, `gradient-diamond`, `circle` and
  `solid` take `kind: scalar`, a canvas-sized scalar field of the number
  they compute before picking a colour: a gradient's position along
  itself, clamped to 0..1 (it then needs no `stops`); a circle's coverage,
  1 inside its hard core falling to 0 at the rim (no `color`); and for
  `solid`, a new `value` everywhere (default 1). `noise` takes
  `kind: sprite` with `width-px` / `height-px`, coloured as in raster mode
  and sampled in the sprite's own pixels, with a fixed default seed, so
  each copy is the same image.
- `gradient-linear`, `gradient-radial`, `gradient-conic` and
  `gradient-diamond` take `interpolation: "step"`, as `color-ramp` does:
  each position takes the colour of the stop at or below it (the first
  stop's below them all), so the gradient paints flat bands. `linear`
  stays the default.
- `circles` takes `stroke-align`: `center` (the default, as before)
  straddles `radius` with the ring, `outside` draws it from `radius` out to
  `radius + stroke-width`, as MapLibre draws `circle-stroke-width`, and
  `inside` keeps it within `radius`. The blur fades to the ring's outer
  edge wherever it sits. `ezu translate` writes `outside` for a layer with a
  stroke, so a stroked circle is as large as MapLibre draws it.
- `fill-solid` takes `edge-alpha` and `edge-alpha-expr`, an opacity for the
  outline alone (default 1), since `fill-alpha` and `opacity-expr` fade the
  fill and leave the outline as it is. `ezu translate` gives the outline the
  layer's `fill-opacity` through them, so a faded fill's outline fades with
  it, as in MapLibre.
- `stroke` takes `dash-unit`: `px` (the default, as before) measures
  `dasharray` in pixels, and `width` in multiples of the stroke's width, as
  MapLibre's `line-dasharray` is, scaled per feature group when the width
  comes from `width-expr`.
- `transform` takes `translate-px` (and `translate-x-px` / `translate-y-px`,
  which take a `$param` or an `@node`): a shift in canvas pixels, converted
  to feature units for the tile being drawn, so it looks the same at every
  tile size. It adds to `translate`, which stays in feature units.
- `stroke` takes `offset-px` and `offset-expr`, MapLibre's `line-offset`:
  the line is drawn that many pixels to the right of its direction of
  travel, or to the left when negative, its corners mitred out to twice
  the offset and bevelled past that. `ezu translate` carries
  `line-offset` over to them; it used to drop it silently.
- `raster` takes `opacity`, `hue-rotate-deg`, `saturation`, `contrast`,
  `brightness-min` and `brightness-max`, each a number, `$param` or `@node`.
  They adjust the imagery with the formulas and in the order of MapLibre's
  raster layer, so the same values give the same pixels. At their defaults
  the bound imagery passes through untouched.
- A `raster` source takes `resampling`: `linear` (the default, as before)
  blends neighbouring tile pixels where a tile is magnified or shrunk onto
  the canvas, and `nearest` repeats the pixel underneath, so each tile
  pixel stays a hard-edged block, as MapLibre's `raster-resampling` does.
  It applies to overzoomed tiles and to tiles whose size differs from the
  canvas's.
- `ezu translate` carries a raster layer's `raster-opacity`,
  `raster-hue-rotate`, `raster-saturation`, `raster-contrast`,
  `raster-brightness-min` and `raster-brightness-max` over to the `raster`
  node, a zoom curve through an `expr` node, and a constant
  `raster-resampling` over to the source. They used to be dropped with a
  warning. When layers over one source want different resampling, the
  later one reads a copy of the source carrying its own.
- `stroke` takes `miter-limit` (MapLibre `line-miter-limit`): how many
  half-widths a `miter` join may reach before it is bevelled. It defaults
  to 4, the limit `stroke` has always drawn with, and may be a `$param` or
  an `@node`. Once written it also bounds the corners of an `offset-px`
  line, which otherwise keep MapLibre's default of 2.

### Changed

- `ezu_paint::StrokeStyle` has a `miter_limit` field; set it to 4.0 to
  draw as before.

- A `switch` branch that is not taken is no longer rendered, nor anything
  only it reads. With a literal or `$param` choice the evaluator leaves it
  out before rendering; with a `@node` choice, every branch that choice
  could lead to still renders. Nodes gain `Node::reads_input` to say which
  inputs a render will read.

- `ezu_paint::host::stitch_padded_raster` and `upsample_subregion_raster`
  take the source's `Resampling` as a last argument; pass
  `Resampling::Linear` to sample as before.
- `ezu translate` writes `anchor-offsets` for `text-variable-anchor` and
  `overlap: "always"` for `text-allow-overlap` / `text-overlap: always`, in
  place of the now-deprecated `anchor-variants`, `radial-offset` and
  `allow-overlap`, so a translated style builds without deprecation
  warnings. The labels land where they did before.
- `ezu translate` carries a data-driven `text-anchor` on a point-placed
  layer, such as a `match` on a property or a `step` on zoom, as the `text`
  node's `anchor-expr`, so each label takes the anchor MapLibre gives it. It
  used to warn and fall back to `center`. `text-variable-anchor` still takes
  precedence, and a line-placed layer still warns and uses the default.
- A vector tile loaded with `load_source` and bound into several source sets
  prepares its features for filtering once per zoom, not once per render that
  reads it, so a style that stitches layers across tile borders no longer
  repeats that work for each of a tile's neighbours. A block of 4×4 stitched
  tiles renders about 30% faster on four threads.
- Every CLI command that draws — `tile`, `tiles`, `bbox`, `frame`, `graph
  --tile` and `serve` — renders through the same renderer as the Rust, Go
  and JS packages, and builds each style's graph once, in that renderer.
  Each source tile is fetched and decoded once and kept for
  the tiles around it and, in `frame --pages`, the pages after it, instead
  of being decoded again for every tile that reads it, and raster and DEM
  tiles are now kept within `EZU_TILE_CACHE_MB` rather than without limit.
  The images and features files are byte-identical to before, except that a
  `dem` or `raster` source with `on-missing: upsample` could come out
  differently from run to run when tiles rendered in parallel, and now comes
  out the same every time.
- `ezu serve` reads tiles from a local directory or archive again for every
  request, so a tile rebuilt on disk is drawn on the next redraw; it used to
  keep the first bytes it read for as long as it ran. A tile whose bytes did
  not change is not decoded again, and tiles from a URL are still fetched
  once.
- `scale-px` on `noise` takes a `$param` that follows the value a render
  passes, or an `@node`, as `warp`'s always has; it used to take a
  `$param`'s default once at build time. `warp` takes the `[x, y]` form
  `noise` has, to stretch its displacement along one axis. Each entry of
  the pair may be a number or a `$param`.
- `spacing-px` on `resample` and `target-px` on `densify` take a `$param`
  that follows the value a render passes, or an `@node`, like the lengths
  on `dash` and `medial-axis`.
- `min` and `max` on `contour` take a `$param` that follows the value a
  render passes, or an `@node`, like `base` and `interval` beside them.
- `scale-px` on `tiling`, `steps` on `posterize` and `amount` on `dither`
  take a `$param` that follows the value a render passes, or an `@node`,
  like `scale` on `place` and `block` on `mosaic`. A `steps` that arrives
  this way is rounded and held to [2, 256]. A `dither` `amount` that is not
  a number, a `$param` or an `@node` now fails the build instead of quietly
  becoming 0.5.
- `ezu translate` turns a `raster-dem` source with `"encoding": "custom"`
  into a `dem` source with `"encoding": "custom"` and the same factors. It
  used to decode any factors other than Terrain-RGB's or Terrarium's as
  Terrain-RGB, with a warning that the heights would be wrong.

### Deprecated

- `anchor-variants` and `radial-offset` on `text` and `text-labels`. They
  still place exactly as before, but the build now warns and spells out the
  `anchor-offsets` list that places the same way. `anchor` with `offset-em`
  stays the way to give a label one fixed position.
- `allow-overlap` on `text` and `text-labels`, for `overlap`: `true` is
  `"always"` and `false` is `"never"`. It still works, with a warning naming
  the replacement; when both are set, `overlap` is used.
- `clip` on `blend`, for `"composite": "source-atop"`, which is what
  `clip: true` did. It still works, with a warning naming the replacement;
  when both are set, `composite` is used. `ezu translate` writes
  `composite` for a `fill-pattern`.
- The two-way form of `switch`, `a` / `b` picked by `select`. Write
  `cases` instead — `"cases": [{ "when": "$fancy", "then": "@fancy" }],
  "default": "@plain"` takes `@fancy` when `$fancy` is true, as `"b"` did
  — or `branches` named `a` and `b` for a `string` param. The old form
  still picks as before, and the build warns with the replacement written
  out for that switch's own inputs. `ezu translate` writes `cases` for a
  background's zoom range and for a layer kept by `--keep-hidden`, which
  is now turned on by setting its case's `when` to `true`.
- `place`'s `top-center`, `center-left`, `center-right` and
  `bottom-center` anchors, for MapLibre's `top`, `left`, `right` and
  `bottom`, the names `text` and `stamp` take. They place the image as
  before, with a warning naming the replacement, also when a `string`
  param's default or `enum` holds one.

### Fixed

- An `mvt` or `pmtiles` source whose `url` was a `file:` path, as `geojson`
  sources and sprites are written, read nothing: the CLI took `file:` as part
  of the path, every tile came back missing, and the map drew empty without a
  warning. `file:` is now taken off and a relative path resolves against the
  style's directory, as for every other `file:` source; a bare path still
  reads relative to the working directory. A local `{z}/{x}/{y}` template
  whose directory does not exist now fails to open, naming the directory,
  instead of drawing every tile empty.
- The CLI panicked at start-up on a machine with no root certificates
  installed, a bare Linux container say, even when every source was a local
  file: the `raster` and `dem` registries built their HTTP client up front,
  and building one loads the system's certificates. The client is now built
  on the first request, and a failure to build it fails that source's fetch
  with the reason instead of aborting.
- `label-boxes` gave one label a different `label-id` on each tile when its
  source carried the feature in more than one tile, as a `geojson` source
  does for every tile and an MVT tile's buffer does near its edge: each
  tile kept a different copy of the label, and the id was taken from the
  copy. It is now taken from what placement treats as the same label (its
  layer, text, style and anchor), so it matches across tiles as documented.
  Ids differ from 0.12.0's.
- `icon-opacity` on `text` and `text-labels` takes a `$param` or an `@node`
  as its schema always said. A `@node` was rejected at build time and a
  `$param` stayed at its declared default whatever the render passed.
- `spacing` on `point-grid` and `point-scatter` takes an `@node` and follows
  a `$param` set at render time, as its schema said; it used to reject the
  one and keep the other at its default. It may now be left out when both
  `spacing-x` and `spacing-y` are given.
- `ezu translate` dropped the `minzoom` / `maxzoom` of `background`,
  `raster` and `hillshade` layers, so they drew at every zoom. They now
  carry over as they do for feature layers: as `min-zoom` / `max-zoom` on
  the `raster` and `dem` nodes, and for a background as a `switch` that
  picks a transparent fill outside the band.
- `ezu translate` dropped a `raster` source's `minzoom` and `maxzoom`, so
  past the source's deepest zoom it asked for tiles that do not exist
  instead of upsampling; it also dropped a `raster-dem` source's
  `minzoom`. They now carry over as `min-zoom` / `max-zoom`, as they do
  for `vector` sources.
- `ezu translate` ignored `hillshade-shadow-color` and
  `hillshade-highlight-color`. A constant colour now becomes the
  `hillshade` node's `shadow-color` / `highlight-color`; an expression
  warns and leaves the node's default.
- `ezu translate` recognised a `custom` `raster-dem` encoding as Terrain-RGB
  or Terrarium only when its `baseShift` was negated (`-10000`, `-32768`).
  MapLibre subtracts `baseShift`, so its own factors for those encodings
  have `10000` and `32768`, and those are now what match.
- The schema lists `$param` and `@node` for `gap-width-px` on `stroke`, which
  has always accepted them; editors that validate against the schema
  flagged them as errors.
- `ezu translate` turned an expression `line-width` on a `line-pattern`
  layer (usually a zoom curve) into a 1 px width and dropped an expression
  `line-opacity`, both without a warning, and skipped a data-driven
  `line-pattern`. They now become the `line-stamp`'s `width-expr`,
  `opacity-expr` and `name-expr`. A `line-gap-width` on such a layer, which
  is not drawn, now warns.
- `ezu translate` ignored `circle-stroke-opacity` and `circle-blur` without a
  warning, and let `circle-opacity` fade the stroke ring, which MapLibre
  leaves alone. They now become the `circles` node's `stroke-opacity` and
  `blur` (or their `*-expr` forms), and a layer that sets `circle-opacity`
  under a ring gets `stroke-opacity: 1`.
- `ezu translate` dropped `icon-anchor` and `icon-offset` from a
  line-placed icon, which it stamps, without a warning. They now become the
  `stamp`'s `anchor` (or `anchor-expr`) and `offset`; an expression
  `icon-offset` warns. The warning for an expression `icon-offset` on a
  point-placed icon named `text-offset`, and now names `icon-offset`.
- `ezu translate` dropped an expression `fill-outline-color` without a
  warning. It now becomes the `fill-solid`'s `edge-expr`, with the 1 px
  outline a constant one gets, and a value it cannot read warns.
- `ezu translate` scaled `line-dasharray` by a 1 px width when `line-width`
  was an expression, usually a zoom curve, so the dashes came out as short
  as a hairline's at every zoom, without a warning. The dash now carries
  over in line widths with `dash-unit: width`, so it scales with the width
  each feature is drawn at, as in MapLibre. A `line-dasharray` given as
  `["literal", […]]` converts too, and one given as an expression, which
  used to be dropped silently, warns and draws solid.
- `ezu translate` ignored `circle-translate`, `fill-translate`,
  `line-translate` and `fill-extrusion-translate` without a warning. A
  constant shift now becomes a `transform` node with `translate-px` in
  front of the paint node; an expression, or a shift on a `fill-pattern`
  or `line-pattern` layer, warns and draws unshifted. The `*-anchor`
  beside each needs nothing, since a tile is never rotated.
- `ezu translate` painted a `background` black when `background-color`
  was an expression, such as a zoom curve, opaque when the colour was
  translucent, and ignored `background-opacity` and `background-pattern`,
  all without a warning. An expression colour now becomes an `expr` node
  feeding the `solid`'s `color`, a constant `background-opacity` and the
  colour's own alpha are folded into the colour, and an expression
  opacity, an opacity beside an expression colour, or a pattern warns.
- `ezu translate` dropped the alpha of a constant `rgba()`, `hsla()` or
  `#rrggbbaa` colour on `fill-color`, `fill-outline-color`,
  `fill-extrusion-color`, `line-color`, `circle-color`,
  `circle-stroke-color`, `text-color` and `text-halo-color`, so a
  translucent colour drew opaque. It now carries over as `#rrggbbaa`, which
  the node's opacity multiplies, as MapLibre's does.
- `ezu translate` ignored, without a warning, any paint or layout property
  its layer converter did not read: `line-blur`, `line-gradient`, the
  `raster-*` paint properties, `fill-extrusion-height`, `text-halo-blur`,
  `icon-color` and others. Each is now named in a warning,
  `` `name` is not supported — ignored ``, and so is
  `fill-antialias: false`. Properties that change nothing on a tile — the
  `*-translate-anchor` and pitch properties, `raster-fade-duration`,
  `symbol-avoid-edges`, and a symbol's `icon-*` properties without an icon
  or `text-*` ones without text — stay quiet.
- `icon-rotate-deg` on `text` and `text-labels` turned the icon about its
  own centre, so an icon with an `icon-anchor` other than `center`, or an
  `icon-offset`, spun in place where MapLibre swings it round the anchor
  point. It now turns about the anchor point, the offset with it, as `stamp`
  already turned its image, and `label-boxes` reports the icon's quad
  there. Icons without rotation, and centred icons without an offset, draw
  as before.
- A turned icon on `text` and `text-labels` collided by its unturned box,
  so an icon turned by `icon-rotate-deg` could overlap a neighbour where it
  reached past that box, or be blocked by one it no longer covered. It now
  collides by the axis-aligned bounds of its padded box turned about the
  anchor point, the envelope MapLibre takes and the one a turned label
  already used, and `label-boxes` reports that box as its `collision` part.
  Placement changes only for symbols whose icon is turned.
- A `raster` source with `resampling: linear`, and every `dem` source,
  sampled its tiles at each canvas pixel's corner rather than its centre
  when a tile's pixels did not land one to one on the canvas, as with a
  512 px tile on a 256 px canvas or at another pixel ratio: the image sat
  half a tile pixel off, and the last pixel before each tile seam repeated
  instead of blending into the neighbour. They now blend the four tile
  pixels around the canvas pixel's centre, as a GPU's linear filter and
  MapLibre's hillshade do, across tile seams too; next to a missing
  neighbour the tile's edge pixel is held rather than blended with the far
  side of the tile. Rasters drawn one to one are unchanged; a DEM drawn one to one
  can differ in the last bits of a tile's last row and column, which were
  read as a near-but-not-exact blend.
- `ezu translate` ignored `line-miter-limit` with a warning, and its
  `stroke` bevelled miter joins only past 4 half-widths where MapLibre's
  default is 2, so sharp corners came out more pointed. A miter-joined line
  now carries the layer's `line-miter-limit`, or 2 when it sets none, as
  the `stroke`'s `miter-limit`; a zoom curve goes through an `expr` node.
  `line-round-limit` still warns.
- `edge-detect` and `sharpen` read the pixels one device pixel either side
  of each pixel, so at pixel ratio 2 they drew edges and overshoot half as
  wide on the map as at ratio 1. Their neighbours now sit one style pixel
  away, read between pixels at a fractional ratio, and a ratio-2 render
  matches the ratio-1 one. A sprite's pixels stay style pixels, and
  renders at ratio 1 are unchanged.

## 0.12.0 — 2026-10-12

### Breaking

- `text` and `text-labels` with point placement (the default) label polygons.
  They used to skip them, so a symbol layer over an area layer drew nothing for
  its areas. Each polygon now gets one label at its pole of inaccessibility,
  the spot inside farthest from its outline, found the way MapLibre's
  `findPoleOfInaccessibility` finds it and to the same precision. As in
  MapLibre, each tile anchors the polygon as that tile clipped it and keeps the
  anchor only when it falls inside the tile; the search runs relative to the
  polygon's first vertex, so the tiles that hold a polygon as a neighbour agree
  on the spot. Past a source's `max-zoom`, the anchor is found on the polygon
  as the source tile holds it, so every overzoomed zoom labels it where the
  source zoom does. **A style that points a point-placed label node at a layer
  with polygons renders new labels**; to keep the old output, give the
  `features` node feeding it `["==", ["geometry-type"], "Point"]` as its
  `filter-expr`. `polygon-anchor` (under Added) chooses a different spot.
- Sprite icons are drawn inside a one-pixel transparent frame, as MapLibre pads
  its atlas entries. An icon whose edge fell on a half pixel — any odd-sized
  icon centred on a whole-pixel anchor — used to repeat its last column or row
  beside itself, and bilinear sampling read past every edge. `stamp`,
  `line-stamp`, `place` and label icons are affected: **icons
  lose that ghost edge, and their outermost pixels move by a level or so**.
- `boundary` closes a vector tile's polygon rings. A ring decoded from a vector
  tile leaves its closing vertex implicit, and `boundary` left out the edge back
  to the start, so every outline drawn from tile data had a gap. **Outlines
  drawn through `boundary` gain that last edge**; GeoJSON rings, which repeat
  the closing vertex, are unchanged. `clip-edges: "drop"` now sees the implicit
  edge too, so a clip edge there is dropped as well.
- A translated MapLibre `line` layer on a polygon source-layer strokes the
  polygons' rings, as MapLibre does; it used to draw nothing for them. The
  translator now routes every line layer, `line-pattern` included, through
  `boundary` with `clip-edges: "drop"`, so the edges a tile encoder adds at the
  tile buffer are left out and no tile seam carries a line. Line geometry passes
  through unchanged. **Re-translated styles draw lines that were missing.**
- Some public Rust structs gained fields, so a struct literal that names every
  field needs updating: `ezu_style::spec::LegendEntry` (`features`),
  `MvtSource` and `PmtilesSource` (`min_zoom`, `max_zoom`),
  `ezu_core::text::TextPaint` (`strikethrough_px`), `TextBlock` (`lines`),
  `ezu_features::mvt::DecodedTile` and `ezu_paint`'s `SharedLayer`
  (`overzoom`). `ezu_paint::legend::stand_in_layer` takes the zoom the swatch is
  drawn at.

### Added

- `polygon-anchor` on `text` and `text-labels`: `"pole"` (the default) or
  `"centroid"`, the polygon's area-weighted centroid with its holes cut out,
  for a label that belongs in the middle of an area, such as a block number on
  an address map. Where the centroid falls outside the polygon (an L or a U),
  the pole is used instead.
- `background-color`, `background-padding` and `background-radius-px` on `text`
  and `text-labels`: a box behind each point label, as big as the laid-out text
  (every line of it) plus the padding, with rounded corners. It is drawn at its
  final size, so it fits "7" and "128" alike without a stretchable sprite.
  `anchor` and `offset-em` place the box rather than the text, so growing the
  padding never moves the anchored corner, and the box is the label's
  collision box. It draws under the halo and glyphs and over the layer's icons.
  `background-padding` alone gives the label the box without painting it, to
  reserve space around a label or measure a badge drawn by other ops.
  Line-placed labels draw without a box.
- `strikethrough`, `strikethrough-expr` and `strikethrough-width` on `text` and
  `text-labels`, which bar each wrapped line of a point label through, in the
  label's colour and opacity and with its halo, for keeping retired names or
  numbers on the map but marked as such. The bar sits at the font's strikeout
  position (0.3 em above the baseline for a `glyphs` stack, which carries no
  metrics) and leaves the collision box alone. Line-placed labels ignore it.
- `label-boxes`, which emits the boxes a `label-placement` node measured its
  labels by as polygons, so geometry and paint ops can draw from them: a
  bordered or rounded badge (`buffer` → `stroke`), an offset shadow
  (`transform`), or a debug overlay of placement. Each box carries the labelled
  feature's properties plus `part` (`text`, `icon`, `background`, `glyph` or
  `collision`), `placed` and a `label-id` shared by every box of one label and
  the same on every tile. `include: "all"` adds the candidates that lost their
  collision test, with `placed: false`. The boxes come from the decisions
  already made, so nothing is placed twice, and labels placed from a
  neighbour's candidates are included, so boxes continue across tile edges.
  The labels cookbook shows a bordered badge and a collision overlay built
  this way.
- `junctions`, which puts a point wherever polylines end or meet, for marking a
  line network split into sections (a route cut into stages, a perimeter cut
  into frontages) with a cross tick at each boundary. Each point carries
  `axis-deg`, the tick's axis — across the line at a loose end or where it runs
  straight on, along the bisector at a corner, through the widest gap at a
  T-junction or a sharp corner — so `stamp` with a bar sprite and
  `"rotation-deg-expr": ["get", "axis-deg"]` draws it, and `arms`, how many line
  ends meet there. `snap-px` merges ends that nearly coincide. Ends a vector
  tile makes by cutting a line at its buffer are dropped, and a junction near a
  tile border comes out the same, axis included, from both tiles.
- `segment-to`, which turns each point into a two-vertex line from a longitude
  and latitude its properties name (`lng-field`, `lat-field`) to the point
  itself: a leader from a label anchor to what it labels, or a connector from a
  building to its entrance. Each line keeps its point's properties, ready for
  `dash` and `stroke`.
- `features` on a legend entry: stand-in features placed by hand, for a swatch
  whose symbol is an arrangement of parts. Each is a GeoJSON-shaped `Point`,
  `LineString` or `Polygon` in `[x, y]` fractions of the swatch, and carries
  its own `properties` over the entry's, so each `features` node's
  `filter-expr` can pick out its part. `ezu check` refuses malformed ones, and
  `ezu legend`'s JSON returns them as declared, for a host that draws its own
  swatches.
- `{ "swatch-x": f }` and `{ "swatch-y": f }` as a legend stand-in's property
  values, which become the longitude or latitude `f` of the way across the
  swatch. A node that reads a position from properties, such as `segment-to`,
  then draws in the swatch as it does on the map. Rust hosts drawing their own
  swatches get the conversion from `ezu::paint::legend` (`stand_in_layer`,
  `swatch_tile`).
- `min-zoom` and `max-zoom` on `mvt` and `pmtiles` sources, as a MapLibre
  source's `minzoom` and `maxzoom`: below `min-zoom` the source's layers draw
  nothing, and past `max-zoom` each tile is cut from the covering ancestor.
  `ezu translate` carries a vector source's `minzoom` and `maxzoom` over.
- `ezu_core::text::place_layers_detailed`, which also reports the label
  candidates that lost their collision test.

### Changed

- The CLI binds every `mvt` and `pmtiles` source a style declares, each at its
  own zoom range; it used to bind only the first, so a style reading two vector
  sources drew the layers of one. `--mvt` and `--pmtiles` stand in for the
  first declared source and leave the others at their own URLs.
- A `dem` source's `tile-size` is marked deprecated in the schema and
  documented as ignored, which it already was: each tile's edge is read from
  its decoded image. Styles that set it still load, and `ezu translate` no
  longer writes it.

### Fixed

- A label straddling a tile seam was missing its far half when the tile across
  the seam had no copy of the label's layer, as happens wherever a vector tile
  encoder leaves out a layer with no features in that tile. That tile now draws
  the neighbour's label as the neighbour does.
- A `file:` glyphs template got its `{fontstack}` percent-encoded, as an
  `http(s)` URL needs, so `Noto Sans Regular` was looked for in a directory
  called `Noto%20Sans%20Regular`. A `file:` path now takes the fontstack as
  written.
- The JSON Schema for style documents left out the `geojson`, `sprite`, `font`
  and `glyphs` source types, so an editor or validator using it rejected every
  style that declared one.
- `ezu translate` passed a `raster-dem` source's MapLibre `encoding` through
  unchanged, so a style using the default `mapbox` encoding (or leaving it out)
  produced a document that failed to parse. `mapbox` now becomes `mapbox-rgb`,
  and a `custom` encoding whose factors match Mapbox's or Terrarium's becomes
  that encoding; any other is read as `mapbox-rgb` with a warning.

## 0.11.0 — 2026-10-06

### Breaking

- Six geometry ops read their pixel lengths as feature-extent units, so on a
  512 px tile every one of them came out an eighth of the size it asked for:
  `hatch` `spacing`, `buffer` `distance`, `simplify` `epsilon`, `densify`
  `target-px`, `resample` `spacing-px`, and `medial-axis` `densify-px` /
  `min-branch-px`. They are canvas pixels now, as documented and as `dash`,
  `wave` and `dot-density` already were. **A style that tuned these values by
  eye against the old behaviour renders differently**: divide them by
  `extent / tile-size` (8, for a 512 px tile at the default extent) to keep the
  old look. The `pencil-sketch` example's `hatch-spacing` is rescaled this way.
- `noise` and `warp` with `type: "worley"` drew each cell's random value,
  clamped so that half the cells came out exactly 1.0, rather than the distance
  field the docs described. `worley` is now that distance, `1 − 2·distance` to
  the nearest site. **A style using `worley` renders differently**; one that
  wanted the flat per-cell tones should switch to the new `cell` type, which now
  spreads them over the whole of `[-1, 1]` — the `stained-glass` example has
  moved to it.
- In the browser, a `collator` or `resolved-locale` expression now fails when
  it is evaluated, where it used to work: the npm package no longer carries
  ICU's collation tables. A style that compares strings with a `collator`
  still loads, then errors at render time. Native builds are unchanged. See
  the wasm module size entry under Changed.

### Fixed

- `color-ramp` painted a field's `nodata` samples through the ramp. A DEM's
  `-9999` sat below the first stop, so a hole in a hypsometric tint came out
  as the lowest stop's colour, deep sea say, instead of a gap. Missing samples
  (NaN, or equal to the field's `nodata`) are now written fully transparent,
  so a hole shows what lies beneath it. NaN was already transparent, but only
  by way of a NaN-to-byte conversion; it now takes the same explicit path. A
  `Raster` input (the luminance gradient map) and a field with neither nodata
  nor NaN render exactly as before.
- `threshold` binarised a field's missing samples along with its real ones. A
  DEM's `nodata` value, `-9999` say, came out as `low`, and NaN as `high` with
  a hard step or NaN with `softness`, while the output still carried the
  field's `nodata`, so a hole turned into ordinary-looking land or sea. Missing
  samples (NaN, or equal to the field's `nodata`) now stay missing: the output
  writes them as the field's `nodata`, or NaN when it has none, as `map-range`
  does. A `low` or `high` equal to the `nodata` value is still written as it
  is, and reads as missing downstream. A field with neither nodata nor NaN
  renders exactly as before.
- `hillshade`, `slope` and `flow-field` read a field's missing samples as
  elevations. Around a DEM's `nodata` hole, `-9999` say, the 3×3 gradient saw
  a cliff, so `hillshade` drew a shadow or highlight rim, `slope` a
  near-vertical ring and `flow-field` large vectors into or out of the hole;
  the hole itself was shaded too, and NaN came out as opaque black in `slope`
  and `hillshade`'s `shade` mode. Missing samples (NaN, or equal to the
  field's `nodata`) are now holes: a missing neighbour counts as the centre
  value, as in `sharpen`, so the ground around a hole reads as level there,
  and a missing pixel is fully transparent in `hillshade` (both modes) and
  `slope`, and the opaque no-flow grey in `flow-field`, which `displace` and
  `flow-smear` leave in place. A field with neither nodata nor NaN renders
  exactly as before.
- `map-range` remapped a field's missing samples along with its real ones. A
  DEM's `nodata` value, `-9999` say, came out as some ordinary-looking number,
  so everything downstream drew it as terrain instead of a gap; NaN turned into
  the output midpoint when `in-min` equalled `in-max`. Missing samples (NaN, or
  equal to the field's `nodata`) now stay missing: the output keeps the field's
  `nodata` and writes them as that value, or NaN when it has none, as `blur`,
  `warp` and `field-math` already do. A field with neither nodata nor NaN
  renders exactly as before.
- A host rendering several tiles against one cache, `ezu bbox` and
  `ezu tiles` among them, could hand a tile the raster its neighbour had cached
  for a world-anchored generator: `noise`, `warp`, `tiling`, `mosaic` and the
  gradients with `anchor: "world"`. Those nodes left the tile out of their cache
  key so adjacent tiles could share them, but each tile's raster is its own
  window onto the field, so a borrowed one was a whole tile out of place. It
  showed as seams, or whole tiles of the wrong texture, wherever the generator
  fed a filter, and changed from run to run because it depended on which tile
  got to the cache first. Every node is keyed by its tile now. A tile rendered
  on its own was never affected.
- `feature-boolean`'s schema called its operation field `op`, the name every
  node already uses to say which op it is, so the field never reached the JSON
  Schema or the node catalog — while the op itself reads `mode`. The schema says
  `mode` now, `xor` included, and the registry refuses (in debug builds) any op
  that declares a field named `op`.
- The CLI wrote its log lines to stdout, where `ezu translate`, `ezu legend` and
  `ezu graph` write the document they produce. A single warning was enough to
  corrupt a redirected recipe — `ezu translate style.json > recipe.json` left a
  file that no longer parsed. Logs go to stderr now, for every command; only
  `check --json` was already spared.
- `ezu tile`, `ezu bbox`, `ezu tiles` and `ezu graph --tile` ignored the
  document's `geojson` sources outright: a style whose features came from
  GeoJSON rendered blank, and said only `no MVT source` on the way. They bind
  it now, exactly as `ezu serve` and the browser already did. The notice also
  names all three feature source kinds, so it stops pointing at MVT when the
  style declares none by design.

### Added

- `field-math`, the functions of `math` applied to scalar fields one pixel at a
  time: add a little scaled noise to an elevation field to roughen it before
  `hillshade`, mix two fields by a third with `lerp`, clamp a field, or scale
  it by a `$param`. Each of `a`, `b` and `c` is either a field or a number — a
  literal, a `$param`, or a node that outputs a number — and a number applies
  to every pixel. At least one operand has to be a field, and every field has
  to be the same size. A pixel is missing in the result wherever any field is
  missing there, or the result is not a finite number (`sqrt` of a negative,
  `div` by zero), and the result keeps the nodata value and geographic scale
  of the first field.
- `sharpen` takes a scalar field as well as a raster or sprite, and gives back a
  field. Sharpening an elevation field before `hillshade` or `slope` steepens
  the flanks of ridges and valleys, so the shading draws crisper crests where
  sharpening the shaded image would only add a halo. The field is not clamped,
  so a sharp step can overshoot its neighbours. It keeps its nodata value and
  geographic scale; a nodata neighbour counts as the centre value, so a hole
  leaves its rim as it was, and nodata samples stay nodata.
- `erode` and `dilate` take a scalar field as well as a raster or sprite, and
  give back a field: each sample becomes the lowest, or highest, value within
  `radius-px`. On an elevation field this generalises the terrain by shape
  rather than by averaging. `erode` narrows peaks and ridges and widens
  valleys, `dilate` does the reverse, and `dilate` then `erode` fills pits and
  gullies narrower than the window while leaving broad slopes as they were. The
  field keeps its nodata value and geographic scale, and nodata samples are left
  out rather than spreading across the window.
- `warp` and `displace` take a scalar field as their `input`, and give back a
  field; `displace`'s `displacement` map stays a raster. Warping an elevation
  field before `contour` makes the contour lines wobble together: they bend as
  one, and never cross, because they are still level lines of a single surface.
  Warping each contour's line afterwards could promise neither. The same works
  on `noise` with `kind: "scalar"`, for a swirled field to threshold or ramp.
  The field keeps its nodata value and geographic scale, nodata samples are left
  out of the interpolation rather than pulling their neighbours towards it, and
  a world-anchored warp stays seamless across tiles.
- `blur` takes a scalar field as well as a raster or sprite, and gives back a
  field. Blurring an elevation field before `hillshade`, `slope` or
  `flow-field` generalises the terrain: small gullies drop out and the major
  ridges stay, so the light and dark sides of a hillshade split along the ridge
  crests. Blurring the shaded image afterwards could not do this; it only
  smeared the shading. The field keeps its nodata value and geographic scale,
  nodata samples are left out of the average rather than pulling their
  neighbours towards it, and the result is the same on every host.
- `flow-smear`, which streaks a texture along a direction raster such as
  `flow-field`'s. Each pixel becomes the average of the input along the
  streamline through it (line integral convolution), and the walk re-reads the
  field at every step, so strokes bend with the terrain instead of fanning out
  in straight lines the way a chain of `displace` and `blend` does. A pixel's
  walk is `length-px` times the field's length there, so flat ground stays sharp
  and steep ground streaks fully; `sides: "forward"` smears one way only, for a
  trail behind each speck, and `taper: "smooth"` fades the stroke ends.
  Smearing a world-anchored `noise` along `dem → flow-field` and thresholding it
  gives hachures down the fall line, or strokes along the contours, seamless
  across tile borders. The upstream pad grows by `length-px` + 1, so an `@node`
  length needs `length-px-max`.
- `flow-field`, which turns an elevation field into a direction raster: which
  way the ground falls at each pixel (`downhill`), rises (`uphill`), or runs
  level along the contours (`along`, with downhill on its right-hand side). The
  vector grows with the slope up to `max-deg` (45° by default), or is full
  length on any slope with `normalize`, and is written in `displace`'s
  encoding — R and G centred on 0.5, flat grey meaning no direction — so it
  drives `displace` as it stands. It takes the same 3×3 Horn gradient as
  `slope` and `hillshade`, `exaggeration` included.
- `stitch` on `features`, which joins a layer to its eight neighbouring tiles
  so its geometry runs on past the tile's edge. A vector tile carries only a
  thin buffer beyond that edge — 16 px on a 512 px protomaps tile — so any
  effect reaching further, such as a wide stroke along a coastline, a large
  `buffer` or a closing, saw the data stop short, or missed a coast lying just
  past the buffer altogether. With `stitch` set, the host fetches the
  neighbours of that layer only, and each tile contributes just the area it
  owns, so nothing where the buffers overlap is drawn twice. `"merge"` unions
  polygons with identical properties, leaving no edge where the tiles met;
  `"pieces"` keeps every piece as its tile cut it, which is cheaper and looks
  the same when the layer is only filled. A neighbour the host cannot provide
  falls back to the tile's own buffer data. The cost is fetching and decoding
  up to eight more tiles for that layer; without `stitch` nothing changes.
- `clip-edges: "drop"` on `boundary`, which leaves out the edges a vector tile
  adds where it clips a polygon at the tile buffer. Those straight edges sit
  just outside the tile, and `boundary` outlined them like a real coastline, so
  a stroke wider than the buffer — a coastal band, say — reached into the tile
  from them and showed as a band along every tile seam. With `drop`, every
  segment that lies entirely past one side of the tile and runs within 15° of
  that side is removed, and a ring is split into open polylines where those
  segments were. The tolerance is what lets the option work after a `buffer`,
  which can bend the clip line slightly where something sits near it. A real
  shoreline outside the tile running nearly parallel to its edge is dropped
  too; nothing inside the tile is removed, and a rectangle exactly on the tile
  border is kept. The default, `keep`, outlines every edge as before.
- Per-cell randomness. Every cell `voronoi-fracture` makes is now its own
  feature carrying `random`, a value in `[0, 1)` drawn from its seed's world
  position, so `["get", "random"]` in a `fill-expr` gives each cell its own
  tone and a cell on a tile border the same tone in both tiles. There was no
  way to do this before: generated geometry had no attribute to key a colour on.
  Two companions make regular cells possible: `aspect` on `voronoi-fracture`
  weights vertical distance (`> 1` stretches cells along X), and `stagger` on
  `point-grid` shifts every other row — together, the bricks of a running bond.
- `cover: "canvas"` on `tile-bounds`, which grows the rectangle over the padded
  canvas. Shapes cut from the bare tile — Voronoi cells, hatch lines — stopped
  at its edge, so a procedural texture showed a seam at every tile border; cut
  from the padded area, both neighbours build the same shape where they meet.
- A Go package, `github.com/reearth/ezu/go`, which embeds the renderer as a
  wasm module and runs it on [wazero](https://wazero.io): pure Go, no cgo, and
  no Rust toolchain to install — the module is committed. It offers the same
  surface the browser bindings do, from the same host-neutral renderer, so a Go
  service and a browser render one style to the same bytes. The wasm module is
  built by `scripts/build-wasm-go.sh` from the new `ezu-cabi` crate, a flat C
  ABI over `ezu-renderer`; neither is published to crates.io.

  A renderer is one wasm instance, and a wasm instance is single-threaded, so
  one renderer serves one goroutine at a time and says so — a concurrent entry
  is refused rather than serialised. Rendering tiles in parallel means several
  renderers, which `ezu.Pool` exists to hold.

  The renderer's warnings — a DEM stitched against neighbours that were never
  bound, a label dropped for want of glyphs, both of which render and return no
  error — go to an `*slog.Logger` given to the runtime with `ezu.WithLogger`,
  from every renderer it makes, pooled ones included. Nobody has to opt in to
  hear that their tile came out wrong.
- `sources()` on the npm package, which answers what a style declares before
  anything has been bound: every source in declaration order, with its kind,
  whether the binding is tile-scoped, where the bytes come from, and — for a
  sprite — the index document to fetch alongside the atlas. A `glyphs` url comes
  back with `{fontstack}` already substituted and percent-encoded, leaving only
  `{range}` to fill in per block, and an inline `geojson` source reports no url,
  which is how it says it needs no binding. `boundSources()` answers the opposite
  question and cannot start a bind loop, so every host was reading the style a
  second time and re-deriving the dispatch `bindSource` already performs. The Go
  package's `Sources()` is the same call, and the bundled demo page now names its
  vector source from it instead of assuming one called `basemap`.
- `--strict` on `ezu check` and on `ezu tile` / `bbox` / `tiles`: a build
  warning fails the run instead of scrolling past. `check --strict` still writes
  its report first, so a CI job gets the findings and the verdict. `ezu serve`
  has no such flag — a live editor that walks out over a warning is no use.
- A source node that resolves its `source` implicitly now says so. A
  `features`, `raster` or `dem` node with no `source` still falls back to the
  document's only source of that type, but the fallback raises a build warning
  naming the node and the source it landed on: the style depends on something it
  never states, and stops meaning the same thing the day a second source is
  declared. `ezu check` reports the warnings (and lists them under `warnings` in
  `--json`), `ezu tile` / `bbox` / `tiles` and `ezu serve` log them, and the
  browser renderer logs them to the console.
- `Graph::warnings()`, the build-time warnings a host can surface, and
  `FactoryCtx::warn` for a node factory to raise one. The warning a field raises
  when it reads a `$param` at build time goes through the same channel, so it
  now names its node and reaches `ezu check --json` too.
- A `geojson` source's `url` is read by the native hosts, not just the browser
  one, and goes through the same resolver as any other document asset:
  `http(s)://`, `file:` (relative to `--assets-dir`) and `data:` all work. The
  document is read once per style rather than per tile, so a pyramid run costs
  one read.
- `ezu_paint::host::GeoJsonSources` and `bind_geojson_sources`, the resolve-once
  / project-per-tile pair every host now shares.

### Changed

- Gaussian blur — the `blur` op and `fill-solid`'s `blur-sigma` — is ezu's
  own, in fixed point, and renders the same bytes natively and in the
  browser. libblur's native SIMD path rounded differently from the scalar one
  wasm runs, so a blurred tile could come out a level apart between the two.
  Blurred output moves by at most one level against before; the speed is
  close to libblur's SIMD path, and libblur is no longer a dependency.
- Every transcendental function on the render path — hillshade and slope, lab
  and hcl colour, sRGB transfer, conic gradients, levels' gamma, density
  kernels, Mercator projection, rotations, brush radii — now comes from the
  `libm` crate rather than `f64::sin` and its kin, which defer to the
  platform's own libm natively and could disagree with wasm in the last bit.
  No disagreement had turned up, so this makes host-independence something the
  code guarantees rather than something the inputs happened to allow, and
  `clippy.toml` now refuses the standard methods so it stays that way. Output
  that goes through these functions can move by a level against before.

  `libm` is slower than a tuned platform libm, so the hot spots no longer call
  it per pixel: `hillshade` shades from the gradient with square roots alone
  (2–6× faster than before), `levels` and the sRGB decode look up the 256
  values an 8-bit channel can take, and `color-ramp` and the gradients convert
  their stops into the interpolation space once per tile rather than per pixel
  (a `hcl` ramp is now faster than before). `mix` in `lab`/`hcl` and
  `gradient-conic`, which still need a transcendental per pixel, run about 1.4×
  slower than they did.
- The npm package's wasm module is a quarter smaller — 6,103,052 → 4,665,571
  bytes, gzipped 2,215,214 → 1,751,120 — by keeping out of the wasm build two
  dependencies that cannot run there: ICU's collation tables, which
  `maplibre-expr`'s `collator` feature carries, and Rayon, which `geo` pulls in
  for a target with no threads. The cost is that a `collator` or
  `resolved-locale` expression now errors at evaluation time in the browser, as
  a `system:` font source already did; native builds are unchanged and render
  the same bytes.
- Expressions are evaluated by `maplibre-expr` 0.5.4, a correctness pass
  collated against the MapLibre style spec. Error messages are now upstream's
  word for word, so the text surfacing from a bad expression field reads
  differently; colour parsing is a real port of CSS Color 4 and is stricter
  about malformed input; and `to-number`, `to-string` and `number-format` follow
  JavaScript more closely. Five ways to crash on ordinary input are fixed,
  including one reachable from `--migrate` on any style carrying an empty
  filter. Its maths goes through `libm` as ezu's own now does, so `sin`, `^`,
  `exponential` curves and `interpolate-hcl` give the same result natively and
  in the browser.

## 0.10.0 — 2026-09-16

### Breaking

- MapLibre styles written in the legacy forms are no longer migrated on the way
  in. `{stops}` function objects and `{token}` label strings pass through
  untouched, each named in a warning, unless you ask for the migration with
  `--migrate` (`ConvertOptions::migrate`), which runs MapLibre's own `migrate`
  over the whole style first. Left alone, a function object is rejected at parse
  time and a token string renders literally. Legacy-form filters are still
  converted unconditionally, as MapLibre's own `createFilter` does.
- `ConvertOptions` gained a public `migrate` field, so a struct literal that
  names every field needs updating.
- `fill-dabs` anchors its lattice to the world rather than to the tile, so dab
  fills render differently: every cell not already sitting on a global boundary
  moves. In exchange, neighbouring tiles agree at any `spacing-px`, where before
  they agreed only when the spacing happened to divide the tile width.

### Added

- Prebuilt `ezu` binaries on every release, for macOS, Linux and Windows on both
  x86_64 and aarch64, and a Homebrew tap that installs them:
  `brew install reearth/tap/ezu`. Neither route needs a Rust toolchain.
- `ezu --version`.
- `ezu graph <style> --tile Z/X/Y --out g.html` writes the graph as an HTML page
  with every node's own intermediate tile drawn on it. Click a node for the
  full-size picture, its op, what it read, and how long it took; ports carrying
  no pixels report what they do carry, such as how many features survived a
  filter or the range of a scalar field. One render produces all of it. Without
  `--tile`, the command emits the same Mermaid it always did, byte for byte.
- `Evaluator::with_observer` and the `NodeObserver` trait, the evaluator hook
  that the above is built on.
- `coord::tile_px_to_world`, the tile-pixel-to-world conversion that callers had
  been rebuilding inline, with the two axes kept separate.

### Fixed

- `strokes` and `stamp` divided world y by the tile *width*, so on a non-square
  canvas the world position they seeded from — and the jitter keyed on it — was
  wrong.
- `next_unit` could return exactly 1.0, roughly one draw in 33 million, because
  `x as f32` rounds the largest top words up to 2^32.

### Changed

- `dem_decode` computes Web Mercator's ground scale with a single `cosh` rather
  than a round trip through a latitude. Same value, one call.
