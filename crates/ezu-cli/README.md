# ezu-cli

Command-line renderer for the Ezu Style Spec — the `ezu` binary of the
[`ezu`](../../README.md) workspace.

```sh
cargo install ezu-cli
```

Point it at any style (URL or local path) and it renders PNGs. A style
declares its own tile sources in a `sources` block (MVT, PMTiles, raster
DEM, RGBA raster, GeoJSON), so most commands need nothing but a
`--style` and a tile address; CLI flags override anything declared there
for one-off swaps.

## Commands

| Command | What it does |
|---|---|
| `ezu tile` | Render a single `z/x/y` tile to PNG (or lossless WebP via `--out tile.webp` / `--format`) |
| `ezu bbox` | Stitch the tiles covering a lon/lat box at one zoom into a single image |
| `ezu tiles` | Bulk-render an XYZ pyramid into `<out>/<z>/<x>/<y>.png` over a zoom range, `--concurrency` tiles at a time |
| `ezu check` | Validate a style — parse + build graph + resolve assets. Exits non-zero on error, so it drops into a pre-commit hook or CI step. `--no-fetch` stays offline (parse + graph only) |
| `ezu translate` | Lower a MapLibre GL style into an ezu recipe via [`ezu-translate`](../ezu-translate). `--font "NAME=SOURCE"` maps a fontstack entry to a real font; skipped or approximated layers are reported on stderr |
| `ezu graph` | Emit a Mermaid `graph LR` diagram of the style's node dependencies |
| `ezu schema` | Print the Ezu Style JSON Schema, assembled from the registered ops (`--out FILE` to write it). Feed it to an editor's JSON language server, an `ajv` CI check, or a docs generator — [custom ops](../ezu-graph#custom-ops) are included |
| `ezu serve` | Live editor + tile server (below) |

Global flags: `--verbose` / `-v` turns on per-node debug logs from the
evaluator (op name, cache hit/miss, output shape, eval duration).
Render commands share `--style`, `--assets-dir`, `--pmtiles` / `--mvt`,
`--overzoom-levels`, and repeatable `--param NAME=VALUE` overrides
validated against the style's `params` declarations.

```sh
# Single tile — the reference styles bundle their own `sources` block,
# so no `--pmtiles` / `--mvt` is needed.
ezu tile --style crates/ezu/examples/styles/watercolor.json \
  --tile 13/7276/3225 --out tile.png

ezu bbox --style URL_OR_PATH \
  --bbox 139.74,35.65,139.78,35.69 --zoom 13 --out tokyo.png

ezu tiles --style URL_OR_PATH \
  --bbox 139.74,35.65,139.78,35.69 --min-zoom 10 --max-zoom 14 --out pyramid

ezu check style.json --no-fetch
ezu translate maplibre-style.json --out recipe.json

# Parameter overrides, validated against the style's `params` block.
ezu tile --style watercolor.json --tile 13/7276/3225 \
  --param 'paper=#ffe0f0' --param softness=2 --out tile.png
```

## `ezu serve` — live editor + tile server

```sh
ezu serve                                                # default example style
ezu serve crates/ezu/examples/styles/pencil-sketch.json  # a specific style
ezu serve https://example.com/style.json                 # or fetch one over http(s)
# Open http://127.0.0.1:8080
```

The server hosts the browser editor at `/`, rendered tiles at
`/tiles/{z}/{x}/{y}.{png,webp}`, raw MVT bytes at `/mvt/{z}/{x}/{y}`,
and a registry-derived JSON Schema at `/schemas/ezu-style.json` — the
same schema the editor validates against as you type, so
[custom ops](../ezu-graph#custom-ops) get autocomplete for free.
`/style/params` serves the current style's parameter schema and
`/style/attribution` the merged source attributions.

The editor (MapLibre GL based) supports:

- **Open / URL / Save** — load a style from a local file or http(s) URL,
  save the current buffer as `<name>.json`. Open on Chromium browsers
  uses the File System Access API so Save writes back in place.
- **Apply** with `⌘↵` / `Ctrl+↵` (works anywhere on the page).
- **Live preview** — when enabled, auto-applies on every keystroke that
  parses + schema-validates + server-validates clean.
- **External-edit reload** — when launched with a local path
  (`ezu serve foo.json`), the server polls the file and pushes
  Server-Sent Events on every change. The editor swaps the buffer
  silently when clean, or surfaces a Reload banner when the user has
  unsaved edits. The `↻ HH:MM:SS` indicator in the toolbar shows the
  last auto-reload. On Chromium, the same watch also runs against
  files opened via the in-browser file picker. Opening a different
  file via `Open…` / `URL…` detaches the server watch for that
  session.
- **Params panel** — controls generated from the style's `params`
  declarations (sliders for bounded numbers, color pickers, toggles).
  Adjustments ride the tile requests as query-string overrides and
  re-render live without touching the style text; `reset` returns to
  the declared defaults. See
  [parametric styles](../ezu-style#params).
- **Source MVT inspector** — toggle a vector overlay of the underlying
  MVT, with per-layer ON/OFF and click-to-inspect feature properties.
  Layers are discovered from the tile at the map center; pan/zoom
  rescans automatically.
- **Tile grid + zoom indicator** — toggle a `z/x/y` boundary overlay
  (drawn per tile via `maplibregl.addProtocol`), and read the live
  zoom value (click to copy `z @ lat,lng`).

A self-contained [`ezu-wasm`](../ezu-wasm) demo page and the routes it
needs (including the COOP/COEP headers the threads build requires) ship
with `ezu serve` too.

## License

MIT or Apache-2.0, at your option.
