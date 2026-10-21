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

Or from the command line, with the `ezu` CLI (`ezu translate style.json
--out recipe.json`) or this crate's example:

```sh
cargo run -p ezu-translate --example convert -- style.json > recipe.json
```

The [layer mapping](https://reearth.github.io/ezu/maplibre/compatibility/)
and [gaps and differences](https://reearth.github.io/ezu/maplibre/gaps-and-differences/)
pages of the documentation hold the full detail; the tables below summarise
them.

### What it converts

| MapLibre | ezu |
|---|---|
| ordered layer list | a `stack` (painter's algorithm: each layer composited source-over onto the ones below), split into a chain of narrow stacks on a long list |
| `background` | `solid`. A zoom curve on `background-color` becomes an `expr` node feeding its `color`; a constant `background-opacity` is folded into the colour's alpha |
| `fill` (solid colour, `fill-outline-color`) | `features` + `fill-solid` (outline → `edge`, or `edge-expr` when data-driven). `fill-opacity` becomes `fill-alpha` / `opacity-expr` and, under an outline, `edge-alpha` / `edge-alpha-expr` too, since MapLibre fades the outline with the fill |
| `line` (+ `line-dasharray`, `line-cap`/`join`, `line-miter-limit`, `line-gap-width`) | `features` → `boundary` (`clip-edges: drop`) → crisp `stroke`, the dash in line widths (`dash-unit: width`). On a polygon source-layer the rings are stroked without the tile-buffer clip edges, as MapLibre does. A miter join carries `line-miter-limit` as `miter-limit`, MapLibre's default of 2 when the layer sets none. A gap renders MapLibre's casing annulus — one stroke's footprint with the corridor knocked out, so joins, caps and dash phase behave like the GL shader |
| `circle` (+ `circle-stroke-*`, `circle-blur`) | `circles`: a crisp disk at each point with its stroke ring, each property a constant or a per-feature `*-expr`. The ring gets `stroke-align: outside`, so it runs from `circle-radius` out to radius + stroke width, as in MapLibre |
| `raster` | `raster`. `raster-opacity`, `-hue-rotate`, `-saturation`, `-contrast`, `-brightness-min` / `-max` become the node's fields, following MapLibre's shader; a zoom curve becomes an `expr` node feeding the field. A constant `raster-resampling` becomes the source's `resampling` |
| `hillshade` (over `raster-dem`) | `dem` + `hillshade` (tone calibration still approximate). A constant `hillshade-shadow-color` / `-highlight-color` becomes `shadow-color` / `highlight-color`. The source `encoding` maps `mapbox`, `terrarium` and `custom` (with its factors) |
| `heatmap` (`-radius`/`-weight`/`-intensity`/`-opacity` incl. expressions; `heatmap-color` over `heatmap-density`) | `features` → `density` (GL-JS kernel) → `color-ramp` with the colour expression baked to a 256-entry ramp per tile (`ramp-expr`); an opacity zoom curve becomes an `expr` node feeding the ramp's `opacity` |
| `fill-extrusion` | flat footprint `fill-solid` with `fill-extrusion-color` (no 3-D — height/base dropped) |
| `symbol` **icons** (`icon-image` incl. data-driven, `icon-size`/`-rotate`/`-opacity`/`-anchor`/`-offset`/`-padding`, `icon-allow-overlap`/`icon-overlap`/`-ignore-placement`, `icon-optional`/`text-optional`, `icon-text-fit` + `-padding` with nine-slice sprite `stretchX`/`stretchY`/`content` metadata) | the icon rides the layer's label: a point symbol's icon and text boxes join the shared collision index and place or drop **as one unit** (`*-optional` lets one half survive alone); a turned icon collides by the envelope of its turned box, as in MapLibre; icon-only symbol layers place too. Line-placed icons lower to a collision-free `stamp`, which keeps `icon-anchor` and a constant `icon-offset` |
| `symbol` **text** (`symbol-placement: point`/`line`/`line-center`: `text-field`, `-size`/`-color`/`-halo-*`/`-opacity`, anchor/offset/justify/wrapping/line height/letter spacing/transform, each layout property as an expression too, a data-driven `text-anchor` on point placement as `anchor-expr`, `text-rotate` on point placement as `rotate-deg` / `rotate-deg-expr`, `text-variable-anchor` (+ `text-radial-offset`, per-anchor offset mirroring) and `text-variable-anchor-offset`, both as `anchor-offsets`, `symbol-spacing`/`text-max-angle`/`text-keep-upright` on lines; collision: `text-allow-overlap`/`-ignore-placement`/`-padding`, `text-overlap`, `symbol-sort-key`) | text labels — point placement labels each point, and each polygon once at its pole of inaccessibility; line placement walks each polyline with tangent-rotated glyphs (per-glyph collision), anchored on tile-clipped geometry with MapLibre's spacing phases. Zero-config: an unmapped `text-font` stack is served from the style's own `glyphs` endpoint as an SDF `glyphs` source (the same pre-rendered glyphs MapLibre draws); `ConvertOptions::fonts` / CLI `--font "NAME=SOURCE"` overrides with a real font per entry for outline rendering — `SOURCE` is a font-file URL (`http(s)://…`, `file:…`, `data:…`) or an installed-font reference (`system:Noto Sans`, optionally `?weight=700&style=italic`); a `system:` source is portable to write but resolves to whatever face the rendering machine has installed (a `{token}` field is an expression once the style is migrated — see below). Collision is **deterministic across tiles** (candidates gathered from the 8 neighbour tiles, deduped, ordered by `symbol-sort-key` with ties broken by tile feature order as in MapLibre, placed greedily) so borders stay seamless. **All of a style's symbol layers collide in one shared index**: each layer lowers to a `text-labels` (candidates) + `text-draw` (its placed labels) pair around a single `label-placement` node, placed top layer first — a POI can knock out a road name, as MapLibre does |
| `fill-pattern` (constant) | `icon` → `tiling`, clipped to the fill shape via `blend { composite: "source-atop" }` |
| `line-pattern` (constant) | `icon` → `line-stamp` (over `boundary`, so polygon rings too), repeated along the line and fitted to `line-width`. An expression `line-width`, `line-opacity` or `line-offset` becomes `width-expr` / `opacity-expr` / `offset-expr` |
| `line-pattern` (data-driven) | `line-stamp` with the style's sprite sheet and the pattern expression as `name-expr`, cropping each feature's icon |
| top-level `sprite` (single URL or `[{id, url}]` sheets; `sheet:icon` names) | one `sprite` source per sheet (atlas `<url>.png` + index `<url>.json`, or inline index) |
| `circle-translate`, `fill-translate`, `line-translate`, `fill-extrusion-translate` (constant `[x, y]`) | a `transform` node with `translate-px` between the features and the paint node, a `line-pattern` layer's `line-stamp` included |
| `line-offset` (constant or expression) | the `stroke`'s, or on a `line-pattern` layer the `line-stamp`'s, `offset-px` / `offset-expr`: the line drawn beside itself, to the right of travel |
| layer `filter` — expression-form (e.g. `["all", ["==", ["get", k], v], ["has", n]]`) **and** legacy-form (bare field names, `!in`/`!has`/`none`) | expression-form passes through verbatim as the `features` node's `filter-expr`; legacy-form is converted to the equivalent expression by `maplibre_expr::convert_legacy_filter` (MapLibre's own pre-compile conversion, strict-type semantics included) — both evaluated by ezu-paint via `maplibre-expr` (full fidelity) |
| zoom / data functions (`interpolate`, `step`, any expression) | emitted raw onto the target node's `*-expr` field (e.g. `fill-expr`, `color-expr`, `width-expr`, `opacity-expr`, `radius-expr`), evaluated per tile by ezu-paint via `maplibre-expr`. A feature an expression gives no value falls back to MapLibre's default for the property (black for `fill-color`, `line-color`, `circle-color`, `text-color`; a transparent text halo) |
| legacy forms — `{stops}` function objects, `{token}` strings in `text-field` / `icon-image`, non-standard `hsl()` colours | converted only with `ConvertOptions::migrate` / CLI `--migrate`, by `maplibre_expr::migrate` (MapLibre's own `migrate`, each value converted with its property spec). Off by default: a legacy form is then passed through untouched and named in a warning — a function object is rejected by ezu-paint at parse time, a token string renders literally. The converter never guesses at a legacy form itself |
| CSS named colours (`steelblue`, `white`, `transparent`, …) | resolved to hex |
| translucent colours (`rgba()`, `hsla()`, `#rrggbbaa`) | `#rrggbbaa`, the alpha multiplied by the node's opacity as MapLibre multiplies it by the layer's |
| `layout.visibility: "none"` | layer dropped (default), or — with `ConvertOptions::keep_hidden` / CLI `--keep-hidden` — kept but gated off behind a `switch` (set its case's `when` to `true` to enable) |
| multiple `vector` sources | all emitted, each with its `minzoom` / `maxzoom` as `min-zoom` / `max-zoom`; each `features` node targets its `(source, layer)` |
| inline / remote `geojson` source (WGS84 lon/lat) | `geojson` source; the host projects it into each tile and binds it as one feature layer (`features` targets `(source, source)`) |
| layer `minzoom` / `maxzoom` | a render-time gate: `min-zoom` / `max-zoom` on the `features`, `raster` or `dem` node the layer reads, and for a `background` a `switch` picked by an `expr` over the zoom |

Recipes are **zoom-independent**: zoom and data functions are emitted as
raw expressions and evaluated per tile (with the tile's zoom in the
`maplibre-expr` context), so a single recipe renders correctly at every
zoom — nothing is baked to a fixed zoom.

### What it does not (yet) — reported in `Report::warnings`

Any paint or layout property the converter neither carries over nor needs
is named in a warning, `` `name` is not supported — ignored ``: among them
`line-blur`, `line-gradient`, `line-round-limit`, the `*-sort-key`
properties of fills, lines and circles, `fill-extrusion-height` / `-base`,
`hillshade-accent-color`, `text-halo-blur`, `icon-color` / `icon-halo-*`,
`text-translate` / `icon-translate`, `text-writing-mode` and
`symbol-z-order`. Properties that change nothing on a tile (the
`*-translate-anchor` and pitch properties, `raster-fade-duration`,
`symbol-avoid-edges`) stay quiet. Beyond those:

- **`text-rotation-alignment: viewport`** on line placement — line-placed
  glyphs always rotate with the line (map alignment). Likewise
  `icon-rotation-alignment` / `icon-pitch-alignment` are ignored.
- **Line-placed icons don't collide** — a `symbol-placement: line` layer's
  icon lowers to a collision-free `stamp`, drawn independently of the
  line's text (point-placed icons collide and pair, see the table).
  `text-rotate` on line placement warns and is dropped.
- Label collision is handled (default on) but **diverges from MapLibre by
  design**: it is world-space deterministic rather than viewport-driven,
  so there is **no "tiles nearest the viewport centre first" priority and
  no per-frame fade in/out**. `text-overlap: cooperative` has no
  equivalent and is treated as `never` (collide) with a warning. A symbol
  carrying both an icon and text takes one overlap decision — the
  conjunction of the icon's and the text's overlap flags.
- **SDF (recolourable) icons** — an `sdf: true` sprite entry is drawn as
  its raw RGBA; `icon-color` tinting isn't applied.
- **Data-driven `fill-pattern`** (only a constant name converts; a
  data-driven `line-pattern` and `icon-image` *are* supported). An
  expression-valued `icon-text-fit-padding`, and with variable anchors an
  expression `text-offset` / `text-radial-offset` or anchor list, warn and
  fall back to constants.
- **`line-gap-width` on `line-pattern` layers** (the gap applies to plain
  `line` layers only), and an expression `line-dasharray`, which draws
  solid.
- An expression `*-translate`, or a `fill-translate` on a `fill-pattern`
  layer, draws unshifted.
- `background-pattern`, and a `background-opacity` that is an expression or
  sits beside an expression `background-color`.
- A zoom expression on `raster-resampling` samples linearly.
- True 3-D **`fill-extrusion`** (the footprint is drawn flat).
- Layers whose `text-font` has no `--font NAME=SOURCE` mapping *and*
  whose style declares no `glyphs` endpoint skip their text.

See [`ezu-compare`](../ezu-compare) to measure how close a converted recipe
lands against a MapLibre reference render.

[MapLibre GL]: https://maplibre.org/maplibre-style-spec/

## License

MIT or Apache-2.0, at your option.
