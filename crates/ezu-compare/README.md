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
default 16), `--stitch` (see below).

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
