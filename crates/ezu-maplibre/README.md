# ezu-maplibre

Convert [MapLibre GL styles] into **ezu recipes** — the node-DAG
[`Document`](../ezu-style) JSON that ezu renders on the CPU (no GPU, no
headless browser).

MapLibre is an ordered list of layers whose paint/layout properties are
computed *per feature* and *per (fractional) zoom* via **expressions**. ezu
is a typed node DAG whose ops are styled uniformly. The two models differ
deeply, so this converter targets the tractable subset first and reports
everything it can't yet reproduce.

```rust
let style: serde_json::Value = serde_json::from_str(&maplibre_style_json)?;
let opts = ezu_maplibre::ConvertOptions { zoom: Some(14.0), ..Default::default() };
let (recipe, report) = ezu_maplibre::convert(&style, &opts)?;
// `recipe` is ezu Document JSON — feed to ezu_style::Document::from_json,
// or write to a .json and render with the `ezu` CLI.
for w in &report.warnings { eprintln!("skipped/approximated: {w}"); }
```

Or from the command line:

```sh
cargo run -p ezu-maplibre --example convert -- style.json 14 > recipe.json
```

## What it converts

| MapLibre | ezu |
|---|---|
| ordered layer list | `blend` chain (painter's algorithm) |
| `background` | `solid` |
| `fill` (solid colour) | `features` + `fill-solid` |
| `fill-color: ["match", ["get", k], …]` | one filtered `fill-solid` per colour bucket (membership filters) |
| `line` | `features` + `brush-solid` + `line` |
| `raster` | `raster` |
| filters `all` / `==` / `!=` / `in` / `!in` | ezu feature filter map |
| zoom functions (`stops`, `interpolate`, `step`) | baked to a constant at [`ConvertOptions::zoom`] |

Because ezu renders one integer zoom per tile, baking a zoom function at
the tile's zoom reproduces MapLibre's value exactly for that tile.

## What it does not (yet) — reported in `Report::warnings`

- **`symbol` (text/icon labels)** — needs glyph rasterization, layout,
  and cross-tile collision. The single largest fidelity gap.
- **Per-feature data-driven paint** other than the `match`-bucket case
  (e.g. `["interpolate", …, ["get", "height"], …]`).
- **Inline / remote GeoJSON sources**, **`fill-extrusion`**, **`heatmap`**,
  **`hillshade`** layers.
- **`line-dasharray`**, line caps/joins, road casing.
- Expression operators outside `all` / `==` / `!=` / `in` / `!in`.

See [`ezu-compare`](../ezu-compare) to measure how close a converted recipe
lands against a MapLibre reference render.

[MapLibre GL styles]: https://maplibre.org/maplibre-style-spec/
[`ConvertOptions::zoom`]: https://docs.rs

## License

MIT or Apache-2.0, at your option.
