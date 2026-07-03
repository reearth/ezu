# ezu-translate

Translate map-engine styles into **ezu recipes** — the node-DAG
[`Document`](../ezu-style) JSON that ezu renders on the CPU (no GPU, no
headless browser).

Map engines describe their maps in their own style languages.
`ezu-translate` lowers those styles into ezu recipes so ezu can render them.
Each engine is a **frontend** exposed as a module; [MapLibre GL] is the
first, under `ezu_translate::maplibre`, and more engines can be added as
sibling modules over time.

## MapLibre frontend (`ezu_translate::maplibre`)

MapLibre is an ordered list of layers whose paint/layout properties are
computed *per feature* and *per (fractional) zoom* via **expressions**. ezu
is a typed node DAG whose ops are styled uniformly. The two models differ
deeply, so this frontend targets the tractable subset first and reports
everything it can't yet reproduce.

```rust
let style: serde_json::Value = serde_json::from_str(&maplibre_style_json)?;
let opts = ezu_translate::maplibre::ConvertOptions::default();
let (recipe, report) = ezu_translate::maplibre::convert(&style, &opts)?;
// `recipe` is ezu Document JSON — feed to ezu_style::Document::from_json,
// or write to a .json and render with the `ezu` CLI. The recipe is
// zoom-independent: one recipe renders correctly at every zoom.
for w in &report.warnings { eprintln!("skipped/approximated: {w}"); }
```

Or from the command line:

```sh
cargo run -p ezu-translate --example convert -- style.json > recipe.json
```

### What it converts

| MapLibre | ezu |
|---|---|
| ordered layer list | `blend` chain (painter's algorithm) |
| `background` | `solid` |
| `fill` (solid colour, `fill-outline-color`) | `features` + `fill-solid` (outline → `edge`) |
| `line` (+ `line-dasharray`, `line-cap`/`join`) | `features` + crisp `stroke` (dash in px) |
| `raster` | `raster` |
| `circle` (+ `circle-stroke-*`) | a `circle` sprite `stamp`ed at each point (stroke = a larger ring stamped underneath) |
| `symbol` **icons** (constant `icon-image`, `icon-size`/`-rotate`/`-opacity`) | `sprite` source → `icon` (crop) → `stamp` at each point |
| `symbol` **text** (point placement: `text-field`, `-size`/`-color`/`-halo-*`/`-opacity`, anchor/offset/justify/wrapping/transform/spacing) | `font` source(s) → `text` node at each point; each `text-font` entry needs a URL via `ConvertOptions::fonts` / CLI `--font "NAME=URL"` (`{token}` fields rewrite to expressions) |
| `fill-pattern` (constant) | `icon` → `tiling`, clipped to the fill shape via `blend { clip: true }` |
| `line-pattern` (constant) | `icon` → `line-stamp` (repeat along the line, fit to `line-width`) |
| `fill-extrusion` | flat footprint `fill-solid` with `fill-extrusion-color` (no 3-D — height/base dropped) |
| top-level `sprite` (single URL or `[{id, url}]` sheets; `sheet:icon` names) | one `sprite` source per sheet (atlas `<url>.png` + index `<url>.json`, or inline index) |
| `hillshade` (over `raster-dem`) | `dem` + `hillshade` (tone calibration still approximate) |
| `heatmap` (`-radius`/`-weight`/`-intensity` incl. expressions; `heatmap-color` over `heatmap-density`) | `features` → `density` (GL-JS kernel) → `color-ramp` with the colour expression baked to a 256-entry ramp per tile (`ramp-expr`) |
| expression-form layer `filter` (e.g. `["all", ["==", ["get", k], v], ["has", n]]`) | passed through verbatim as the `features` node's `filter-expr`, evaluated by ezu-paint via `maplibre-expr` (full fidelity) |
| zoom / data functions (`stops`, `interpolate`, `step`, any expression) | emitted raw onto the target node's `*-expr` field (e.g. `fill-expr`, `color-expr`, `width-expr`, `opacity-expr`, `radius-expr`), evaluated per tile by ezu-paint via `maplibre-expr` |
| CSS named colours (`steelblue`, `white`, `transparent`, …) | resolved to hex |
| `layout.visibility: "none"` | layer dropped (default), or — with `ConvertOptions::keep_hidden` — kept but gated off behind a `switch` (flip its `select` to `b` to enable) |
| multiple `vector` sources | all emitted; each `features` node targets its `(source, layer)` |
| inline / remote `geojson` source (WGS84 lon/lat) | `geojson` source; the host projects it into each tile and binds it as one feature layer (`features` targets `(source, source)`) |
| layer `minzoom` / `maxzoom` | the `features` node's `min-zoom` / `max-zoom` render-time gate (the layer draws only for `min-zoom <= z <= max-zoom`) |

Recipes are **zoom-independent**: zoom and data functions are emitted as
raw expressions and evaluated per tile (with the tile's zoom in the
`maplibre-expr` context), so a single recipe renders correctly at every
zoom — nothing is baked to a fixed zoom.

### What it does not (yet) — reported in `Report::warnings`

- **`symbol` text on lines** (`symbol-placement: line`/`line-center`),
  **collision handling**, and **`text-variable-anchor`** — point-placed
  labels draw, but nothing hides overlapping ones yet. Layers whose
  `text-font` has no `--font NAME=URL` mapping skip their text.
- **SDF (recolourable) icons** — an `sdf: true` sprite entry is drawn as
  its raw RGBA; `icon-color` tinting isn't applied.
- **Data-driven `icon-image` / `fill-pattern` / `line-pattern`** (only a
  constant name converts). Data-driven *values* — fill/line/circle
  colour/opacity/width/radius, `symbol` `icon-size`/`-rotate`/`-opacity`,
  and the text paint properties — *are* supported, emitted as `*-expr`.
- **`heatmap-opacity`** (the layer converts; its opacity is ignored, like
  `raster`/`hillshade` layer opacity); true 3-D **`fill-extrusion`** (the
  footprint is drawn flat).
- Road **casing** (the darker under-stroke MapLibre draws beneath a line).
- **Legacy-form filters** (bare field names, e.g. `["==", "class", "primary"]`,
  and `!in` / `!has` / `none`) — vanishingly rare in modern styles; the layer
  is left unfiltered and a warning is reported. Use the expression form
  (`["==", ["get", "class"], "primary"]`), which converts with full fidelity.

See [`ezu-compare`](../ezu-compare) to measure how close a converted recipe
lands against a MapLibre reference render.

[MapLibre GL]: https://maplibre.org/maplibre-style-spec/

## License

MIT or Apache-2.0, at your option.
