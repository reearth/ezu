# Changelog

Every published crate and the npm package share one version and are released as
a set. Each version's section below is what its
[GitHub release](https://github.com/reearth/ezu/releases) says; releases before
0.10.0 predate this file and have only their commit log.

ezu is pre-1.0, so a minor bump may carry a breaking change. What the numbers
mean is spelled out in
[versioning](https://reearth.github.io/ezu/reference/changelog/).

## Unreleased

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
