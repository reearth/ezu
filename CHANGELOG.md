# Changelog

Every published crate and the npm package share one version and are released as
a set. Each version's section below is what its
[GitHub release](https://github.com/reearth/ezu/releases) says; releases before
0.10.0 predate this file and have only their commit log.

ezu is pre-1.0, so a minor bump may carry a breaking change. What the numbers
mean is spelled out in
[versioning](https://reearth.github.io/ezu/reference/changelog/).

## Unreleased

### Fixed

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
