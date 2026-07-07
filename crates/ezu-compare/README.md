# ezu-compare

Internal bench for the MapLibre frontend of [`ezu-translate`](../ezu-translate): take a
MapLibre GL style, **convert** it to an ezu recipe, **render** it on the
CPU with ezu (timed), and **pixel-compare** the result against a MapLibre
reference render — per XYZ tile, with metrics and a diff image.

This crate is `publish = false` — it's a workspace dev/benchmark tool, not
a published library.

## How it works

For each requested tile `z/x/y`:

1. Convert the style at `zoom = z` (so baked zoom-functions are exact).
2. Render the recipe **in-process** with ezu; time only the graph
   evaluation (MVT fetch/decode excluded), so the number reflects ezu's
   CPU rendering cost.
3. Obtain the reference: either read `--ref-dir/<z>_<x>_<y>.png`, or run
   the [`tools/mlgl-ref`](../../tools/mlgl-ref) Node renderer
   (maplibre-gl-js in headless Chromium, software WebGL — no GPU).
4. Compare → RMSE, mean-abs-error, "visibly different" pixel fraction, max
   channel delta, **SSIM** (structural similarity over 8×8 luma windows —
   less dominated by antialiasing/label noise than RMSE), and a 0–100
   closeness score; write `ezu` / `ref` / `diff` PNGs and the recipe.

The reference is treated as an opaque PNG source, so a maplibre-**native**
generator can be swapped in later without touching the comparison code.

## Usage

```sh
# One-time: set up the reference renderer.
cd tools/mlgl-ref && npm install && npx playwright install chromium && cd ../..

# Compare + benchmark a handful of tiles.
cargo run --release -p ezu-compare -- \
  --style crates/ezu-compare/samples/protomaps-basemap.json \
  --tiles 14/14550/6452,15/29101/12904 \
  --out out/compare
```

Flags: `--style <path|url>`, `--tiles z/x/y,…`, `--out <dir>`,
`--ref-dir <dir>` (use precomputed references instead of the Node
renderer), `--refgen-dir <dir>` (default `tools/mlgl-ref`),
`--threshold <0-255>` (per-channel delta counted as "visibly different",
default 16), `--stitch` (see below), `--bench` (timing only, no reference —
see below), `--repeat <N>` (bench: render each tile N times, keep the fastest).

### `--bench` — timing only, no reference

For a performance pass you often just want to know **where ezu spends its
time**, without setting up the Node reference renderer. `--bench` converts the
style, renders each tile in-process, and prints a per-op and per-node timing
breakdown — reference fetch and pixel comparison are skipped entirely.

```sh
cargo run --release -p ezu-compare -- \
  --style crates/ezu-compare/samples/protomaps-basemap.json \
  --tiles 14/14550/6452,12/3637/1613 \
  --bench --repeat 5
```

Per-node timings come from ezu-graph's `ezu_graph::eval` tracing stream (each
node reports its op and eval time on a cache miss), captured by a tracing
layer — so nothing about ezu's public API changes.

`--repeat N` (default 1) renders each tile N times and keeps the **fastest**
run (lowest wall-clock). Every pass uses a fresh cache, so each node stays a
cache miss and the numbers reflect real work rather than memoised lookups.

For each tile the output has:

- a header with total eval time, wall-clock, and node count;
- an **op table**: op / count / total ms / avg ms / share%;
- the **slowest 15 nodes**: ms / op / node id.

With more than one tile, a combined op table across all tiles is printed at the
end. Sample:

```
=== 14/14550/6452 ===  eval 985.8 ms  wall 986.0 ms  (25 nodes)
op                  count   total ms    avg ms   share%
stroke                  3     516.86   172.286    52.4%
fill-solid              5     428.34    85.668    43.5%
blend                   8      38.99     4.874     4.0%
features                8       1.48     0.185     0.1%
solid                   1       0.13     0.128     0.0%

slowest nodes (top 15):
        ms op                 node
   413.455 fill-solid         landuse__fill
   297.482 stroke             roads_minor__stroke
   169.955 stroke             roads_major__stroke
   ...
```

### `--stitch` — 3×3 tile neighbourhood

By default ezu-compare binds the single centre MVT tile, while
maplibre-gl-js renders a viewport that pulls in neighbouring tiles at the
edges. `--stitch` merges the 3×3 neighbourhood into the centre tile's
coordinate frame (offsetting each neighbour by `±extent`), so geometry
just outside the tile fills ezu's pad ring — the same thing the host does
for DEM/raster sources.

Because the output is cropped to the tile, this only changes **pad-sampling
ops (`blur` / `warp` / `dab`) near tile edges**; plain `fill`/`line` output
is unchanged. It also multiplies decode/render cost (~9× geometry), so it's
opt-in and off by default.

## Sample output

```
tile        score   ssim     rmse   diff%  maxΔ   ezu(ms)
------------------------------------------------------------
14/14550/6452  93.42  0.798   16.777  22.69%    88     262.9
15/29101/12904  94.37  0.890   14.363  17.71%    90     138.7
```

The `diff` PNG (bright = disagreement) makes the residual obvious: at high
zoom it's dominated by missing text labels, line thickness/antialiasing,
and dashes — exactly the parts `ezu-translate` does not convert yet.

## License

MIT or Apache-2.0, at your option.
