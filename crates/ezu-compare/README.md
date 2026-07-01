# ezu-compare

Dev tool that closes the loop on [`ezu-maplibre`](../ezu-maplibre): take a
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
   channel delta, and a 0–100 closeness score; write `ezu` / `ref` / `diff`
   PNGs and the converted recipe.

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
default 16).

## Sample output

```
tile        score     rmse   diff%  maxΔ   ezu(ms)
----------------------------------------------------
14/14550/6452  93.42   16.777  22.69%    88     283.8
15/29101/12904  94.37   14.363  17.71%    90     187.9
```

The `diff` PNG (bright = disagreement) makes the residual obvious: at high
zoom it's dominated by missing text labels, line thickness/antialiasing,
and dashes — exactly the parts `ezu-maplibre` does not convert yet.

## License

MIT or Apache-2.0, at your option.
