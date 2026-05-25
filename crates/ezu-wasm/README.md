# ezu-wasm

WebAssembly bindings for the `ezu` painterly map renderer.

The JS side owns all I/O (HTTP, PMTiles, asset fetching). This crate
exposes a stateful [`Renderer`](src/lib.rs) that holds a parsed style
document, its built graph, an in-memory brush bank, and a per-tile
binding buffer that mirrors the style's `sources` block.

## API

```ts
function simdEnabled(): boolean;

class Renderer {
  constructor(styleJson: string);

  setStyle(styleJson: string): number;            // → new node count
  readonly tileSize: number;

  // Bind raw tile bytes under a `sources.<name>` entry. The renderer
  // dispatches on the source's declared `type`:
  //   - brush  → parse `.myb` JSON, register in the persistent bank
  //              under `decl.src` (clearSources does NOT clear it)
  //   - image  → decode PNG/WebP, persistent same as brush
  //   - mvt / pmtiles → decode MVT, bind as `tile.<layer>` at render
  //                     time, cleared by `clearSources`
  //   - dem    → decode + 3×3 stitch at render time. Bind each
  //              neighbour with `{ coord: [dx, dy] }` (dx, dy ∈ {-1, 0, 1};
  //              the centre is `[0, 0]`, the default).
  bindSource(name: string, bytes: Uint8Array,
             opts?: { coord?: [number, number] }): void;

  // Drop every pending tile-scoped binding. Document-scoped sources
  // (brush / image) keep their bank entries.
  clearSources(): void;

  // Names with at least one pending tile-scoped binding.
  boundSources(): string[];

  // Single unified render. Format and canvas overrides go in `opts`.
  renderTile(z: number, x: number, y: number, opts?: {
    format?: "png" | "webp" | "rgba";       // default "png"
    tileSize?: number;                       // override style canvas
    pad?: number;
    png?: { compression?: "fast" | "default" | "best" };
  }): Uint8Array;

  free(): void;
}
```

### Output formats

`opts.format` picks the encoder:

- **`"png"`** (default) returns PNG bytes — feed to `<img>` via
  `URL.createObjectURL(new Blob([buf], { type: "image/png" }))`.
- **`"webp"`** returns lossless WebP bytes (~15 % smaller than PNG
  on painterly tiles). Pure-Rust encoder, no native deps.
- **`"rgba"`** returns straight (un-premultiplied) 8-bit RGBA bytes
  (`tile_size * tile_size * 4`) — feed directly to
  `ctx.putImageData(new ImageData(new Uint8ClampedArray(buf.buffer), w, h), 0, 0)`
  and skip the PNG decode round trip.

### Per-call size override

`opts.tileSize` and `opts.pad` override the style-level canvas
geometry for that call. Useful for hi-DPI preview rendering without
mutating the style.

### Encoding options

PNG accepts a compression preset via `opts.png.compression`:

```js
// Fast preset — biggest files, lowest CPU. Good for live-preview redraws.
const fast = r.renderTile(z, x, y, { png: { compression: "fast" } });

// Best preset — smallest files, ~2-4× the CPU. Good for cached pyramids.
const small = r.renderTile(z, x, y, { png: { compression: "best" } });
```

WebP is lossless via the pure-Rust `image-webp` codec and has no
quality knob — see below for the lossy-WebP recipe without C deps.

#### Lossy WebP without C bindings

The WASM build stays pure-Rust on purpose (no `libwebp`, no native
linker, smaller `.wasm`). If you want lossy WebP, ask the **browser**
to do it after rendering as `"rgba"`:

```js
async function renderTileToLossyWebp(r, mvt, z, x, y, quality = 0.8) {
  const w = r.tileSize;
  r.clearSources();
  if (mvt) r.bindSource("basemap", mvt);
  const rgba = r.renderTile(z, x, y, { format: "rgba" });
  const oc = new OffscreenCanvas(w, w);
  const ctx = oc.getContext("2d");
  ctx.putImageData(new ImageData(new Uint8ClampedArray(rgba.buffer), w, w), 0, 0);
  const blob = await oc.convertToBlob({ type: "image/webp", quality });
  return new Uint8Array(await blob.arrayBuffer());
}
```

### Missing tiles

Don't `bindSource` an MVT source for that tile — `renderTile` returns
the style's paper background. `features` source nodes see no
`tile.<layer>` binding and emit an empty layer, so downstream paint
nodes short-circuit.

### Errors

Every fallible method throws a JavaScript `Error` whose `.name`
discriminates the failure kind:

| `.name` | When |
|---|---|
| `InvalidStyle` | `new Renderer(...)` / `setStyle(...)` rejected the JSON |
| `BrushParse`   | `bindSource` on a brush source couldn't parse the `.myb` JSON |
| `MvtDecode`    | `bindSource` (mvt/pmtiles) or render couldn't decode the MVT bytes |
| `DemDecode`    | `bindSource` (dem) couldn't decode the raster-DEM tile |
| `UnknownSource`| `bindSource` got a name not in the style's `sources` block, or `coord` was malformed |
| `RenderFailed` | A node `eval` failed (e.g. missing brush, downcast mismatch) |
| `PngEncode`    | PNG encoding failed |
| `WebpEncode`   | WebP encoding failed |

```js
try {
  await loadAndRender();
} catch (e) {
  if (e.name === "InvalidStyle") showStyleError(e.message);
  else if (e.name === "MvtDecode") showFetchError(e.message);
  else throw e;
}
```

## Usage

```js
import init, { Renderer, simdEnabled } from "./ezu_wasm.js";

await init();
console.log("SIMD?", simdEnabled());

// `new Renderer(...)` pre-registers every built-in brush bundled
// with `ezu-paint` (the watercolor + pencil set, CC0). Styles can
// reference these with `"src": "builtin:NAME"`; bring your own by
// declaring a `brush` source in the style and calling `bindSource`.
const r = new Renderer(await (await fetch("/style")).text());

const z = 13, x = 7276, y = 3225;
const mvt = new Uint8Array(await (await fetch(`/mvt/${z}/${x}/${y}`)).arrayBuffer());

// Fast path: bind the source, render as RGBA, blit to a <canvas>.
r.clearSources();
r.bindSource("basemap", mvt);
const rgba = r.renderTile(z, x, y, { format: "rgba" });
const w = r.tileSize;
const ctx = canvas.getContext("2d");
ctx.putImageData(new ImageData(new Uint8ClampedArray(rgba.buffer), w, w), 0, 0);
```

## Logging

The renderer emits per-node `tracing` events (op name, cache hit/miss,
output shape, eval duration). To surface them in the browser, install
a `LogSink` once at startup:

```js
import init, { Renderer, LogSink } from "./ezu_wasm.js";
await init();

// Install once. The filter string is the same `EnvFilter` syntax the
// CLI takes — e.g. "debug" or "info,ezu_graph::eval=debug".
const sink = new LogSink("info,ezu_graph::eval=debug");

// Option 1 — live forwarding (mirrors the CLI's `--verbose` output):
sink.onEvent((e) => console.debug(`${e.target}: ${e.message}`, e.fields));

// Option 2 — drain on demand (for a UI panel):
function refreshLogPanel() {
  for (const line of sink.drainLines()) appendToPanel(line);
}
setInterval(refreshLogPanel, 500);

// Or pull structured records directly:
for (const r of sink.drain()) {
  // r = { level, target, message, fields, timestampMs }
}
```

API surface:

```ts
class LogSink {
  constructor(level: string);                // EnvFilter syntax
  onEvent(cb: ((e: LogRecord) => void) | null): void;
  drain(): LogRecord[];                       // structured, clears buffer
  drainLines(): string[];                     // pre-formatted, clears buffer
  clear(): void;
  setCapacity(cap: number): void;             // default 4096; ring buffer
  readonly len: number;
}
type LogRecord = {
  level: "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR";
  target: string;             // e.g. "ezu_graph::eval"
  message: string;
  fields: Record<string, string>;
  timestampMs: number;
};
```

`new LogSink(...)` is idempotent — the underlying global subscriber is
installed only on the first call. The `level` argument is honoured on
first install and ignored afterwards; reconstructing the sink later
just hands you a new handle to the same buffer.

## Building

`wasm-pack` is the smoothest path. Two builds — one scalar, one with WebAssembly
SIMD — sit side by side under `target/wasm/`:

```sh
cd crates/ezu-wasm

# Scalar
wasm-pack build --target web --release --out-dir ../../target/wasm/scalar

# SIMD (+simd128). wasm-opt is also invoked with --enable-simd via the
# [package.metadata.wasm-pack.profile.release] block in Cargo.toml.
RUSTFLAGS="-C target-feature=+simd128" \
  wasm-pack build --target web --release --out-dir ../../target/wasm/simd
```

Each output directory contains `ezu_wasm.js` (ES module + glue),
`ezu_wasm_bg.wasm`, and TypeScript declarations.

## Demo page

A self-contained demo lives at [`examples/wasm-demo/index.html`](examples/wasm-demo/index.html). It picks
between the scalar and SIMD builds at runtime, between PNG and RGBA output,
renders a single MVT tile on a `<canvas>`, and runs a bench-of-N for timing.
It needs the routes that the `ezu serve` subcommand of
[`ezu-cli`](../ezu-cli) provides out of the box:

| Path | Source |
|---|---|
| `GET /style` | Current Ezu Style JSON |
| `GET /mvt/{z}/{x}/{y}` | Raw decompressed MVT bytes |
| `GET /wasm/{scalar\|simd}/ezu_wasm.js` | This crate's wasm-pack outputs |
| `GET /wasm-demo/` | This `examples/wasm-demo/` directory |

To run end-to-end from a clean tree:

```sh
# 1. Build both WASM flavors (see above)
# 2. Start the server (serves the editor, the static dirs, and the demo)
cargo run --release -p ezu-cli -- serve
# 3. Open http://127.0.0.1:8080/wasm-demo/
```

## Benchmark (M-series Mac, Chrome)

Same tile (`z=13`, central Tokyo, `7276/3225`), 12-sample median, MVT cached
client-side so timings reflect pure WASM render time:

| Build  | Output | binary | min | **p50** | mean | max |
|---|---|---|---|---|---|---|
| Scalar | PNG  | 729 KB | 1055 | **1058** ms | 1058 | 1066 |
| Scalar | RGBA | 729 KB | 1012 | **1016** ms | 1017 | 1025 |
| SIMD   | PNG  | 634 KB | 972  | **976**  ms | 978  | 990  |
| SIMD   | RGBA | 634 KB |  934 | **943**  ms | 942  | 950  |

Two axes of improvement:

- **SIMD vs scalar**: ~1.08× on this watercolor style. Bulk of the work is
  hokusai's `Brush::stroke_to` (`libmypaint` emulation, mostly scalar
  f32 dynamics), so SIMD has narrow surface area to vectorize. Heavier
  dab-fill or blur layers would show a larger gap.
- **RGBA vs PNG**: skipping PNG encode saves ~40 ms on this tile —
  ~3-4% of total. Useful when feeding straight to `putImageData`.

The SIMD build emits **13,367 `v128` instructions** vs **0** in the scalar
build (verified via `wasm-objdump`).

Native (release binary, same workload, same machine): ~280 ms. WASM is
~3.4× slower than native here — consistent with the libmypaint kernel
being the hot loop.

## Caveats

- `wasm32-unknown-unknown` has no monotonic clock, so any
  `std::time::Instant::now()` call panics at runtime. `ezu-paint`'s
  per-layer trace is gated on `target_arch = "wasm32"`; if you add more
  timing in the rendering crates, gate it the same way.
- The `panic-hook` feature is on by default and forwards panics to
  `console.error`. Turn it off for tiny size-sensitive builds.
- All `Vec<u8>` return values are copied across the wasm-bindgen boundary
  when crossing into JS — that's unavoidable today. The RGBA buffer is
  1 MB for a 512×512 tile, which is comparable to a PNG decode on the JS
  side, so prefer RGBA only when you'd otherwise re-decode the PNG into
  an `ImageBitmap`.

## License

Same as the rest of the workspace: MIT or Apache-2.0, at your option.
