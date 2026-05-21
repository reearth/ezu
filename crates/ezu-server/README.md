# ezu-server

> **Unpublished** (`publish = false`) — reference / development server, not a
> library. See the main [README](../../README.md) for the full project overview.

Live editor + tile server for the [`ezu`](../../README.md) painterly map
renderer. Edit an Ezu Style JSON document in a textarea on the left,
click **Apply**, and watch the Leaflet map on the right re-render with
the new style.

## Run

```sh
cargo run --release -p ezu-server
# default: http://127.0.0.1:8080
```

CLI flags (all also overridable via env):

| Flag | Env | Default |
|---|---|---|
| `--pmtiles-url` | `EZU_PMTILES_URL` | `https://build.protomaps.com/20260520.pmtiles` |
| `--style` | `EZU_STYLE` | `crates/ezu/examples/watercolor-basic.json` |
| `--brushes` | `EZU_BRUSHES` | `assets/brushes` |
| `--bind` | `EZU_BIND` | `127.0.0.1:8080` |

## Endpoints

| Method | Path | Description |
|---|---|---|
| `GET`  | `/` | Inline HTML editor (Leaflet + textarea, `⌘↵` to apply) |
| `GET`  | `/style` | Current style as raw JSON |
| `PUT`  | `/style` | Validate + replace style; returns `{ "version": N }` |
| `GET`  | `/tiles/{z}/{x}/{y}.png` | Render the tile under the current style |
| `GET`  | `/mvt/{z}/{x}/{y}` | Raw decompressed MVT bytes (for the WASM demo) |
| `GET`  | `/schemas/ezu-style.json` | JSON Schema derived from the live node registry |

Static directories are auto-mounted when present:

| Path | Source |
|---|---|
| `/wasm-demo` | `crates/ezu-wasm/www` |
| `/wasm/scalar` | `target/wasm/scalar` |
| `/wasm/simd` | `target/wasm/simd` |
| `/assets` | `assets` |

## Style versioning

Each `PUT /style` parses the JSON, re-builds the graph (via the
`ezu-paint` node registry), and atomically swaps the snapshot. The
intermediate result cache lives **inside** the snapshot, so an edit
invalidates everything in one shot — no stale buffers leak across
versions.

## Caching

Upstream MVT bytes are cached in process so editing the style re-renders
without refetching the PMTiles archive. The cache lives for the server's
lifetime; restart to clear.

## Schema

`GET /schemas/ezu-style.json` returns a JSON Schema derived from the
live node registry. Adding a new op via `NodeRegistry::register` and
giving it a `NodeFactory::schema()` implementation surfaces it in the
editor's autocomplete with no further wiring.
