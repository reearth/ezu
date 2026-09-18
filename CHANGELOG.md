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

- The npm package's wasm module is a quarter smaller — 6,103,052 → 4,665,571
  bytes, gzipped 2,215,214 → 1,751,120 — by keeping out of the wasm build two
  dependencies that cannot run there: ICU's collation tables, which
  `maplibre-expr`'s `collator` feature carries, and Rayon, which `geo` pulls in
  for a target with no threads. The cost is that a `collator` or
  `resolved-locale` expression now errors at evaluation time in the browser, as
  a `system:` font source already did; native builds are unchanged and render
  the same bytes.
- Expressions are evaluated by `maplibre-expr` 0.5.2, a correctness pass
  collated against the MapLibre style spec. Error messages are now upstream's
  word for word, so the text surfacing from a bad expression field reads
  differently; colour parsing is a real port of CSS Color 4 and is stricter
  about malformed input; and `to-number`, `to-string` and `number-format` follow
  JavaScript more closely. Five ways to crash on ordinary input are fixed,
  including one reachable from `--migrate` on any style carrying an empty
  filter.

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
