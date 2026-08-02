# ezu-wasm

WebAssembly bindings for the `ezu` painterly map renderer.

The JS side owns all I/O (HTTP, PMTiles, asset fetching). This crate
exposes a stateful [`Renderer`](src/lib.rs) that holds a parsed style
document, its built graph, an in-memory brush bank, and a per-tile
binding buffer that mirrors the style's `sources` block.

## API

```ts
function simdEnabled(): boolean;
// True when compiled with the `threads` feature. When true and the page
// is cross-origin isolated, `initThreadPool` is available and rendering
// can run in parallel; otherwise the renderer is single-threaded.
function threadsEnabled(): boolean;
// Present only in `threads` builds. Spins up the Web Worker pool; await
// it once after `init()`, then pass `{ parallel: true }` to `renderTile`.
function initThreadPool(numThreads: number): Promise<void>;

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
  //   - raster → RGBA imagery tiles (PNG/WebP/JPEG); decode + 3×3
  //              stitch at render time, same `coord` convention as dem
  bindSource(name: string, bytes: Uint8Array,
             opts?: { coord?: [number, number] }): void;

  // Attribution declared by the style (document + sources), joined
  // with ` | `; undefined when none is declared. Upstream TileJSON /
  // PMTiles metadata is the JS host's concern — merge it on that side.
  readonly attribution: string | undefined;

  // Drop every pending tile-scoped binding. Document-scoped sources
  // (brush / image) keep their bank entries.
  clearSources(): void;

  // Names with at least one pending tile-scoped binding.
  boundSources(): string[];

  // Neighbour offsets the style actually reads for this source, as
  // [dx, dy] pairs (never [0, 0]). Only cross-tile label collision and
  // edge-continuous DEM shading read neighbours, and only for the
  // sources that need them — fetch these instead of the whole 3×3.
  // Empty means the centre tile is enough.
  requestedNeighborOffsets(name: string): [number, number][];

  // Codepoints the bound features can require, keyed by `glyphs`
  // source name, sorted. For hosts that can build their own glyph PBF
  // holding exactly these — far less to transfer than whole ranges.
  neededCodepoints(): Record<string, number[]>;

  // The same set rounded out to whole ranges: each number is a range
  // start, so the `{range}` in `…/{fontstack}/{range}.pbf` is
  // `${start}-${start + 255}`. For hosts fetching off a stock MapLibre
  // glyphs endpoint. Call either after binding the vector sources and
  // before rendering — this host cannot fetch glyphs lazily. Both
  // over-approximate (see below).
  neededGlyphRanges(): Record<string, number[]>;

  // Single unified render. Format and canvas overrides go in `opts`.
  renderTile(z: number, x: number, y: number, opts?: {
    format?: "png" | "webp" | "rgba";       // default "png"
    tileSize?: number;                       // override style canvas
    pad?: number;
    png?: { compression?: "fast" | "default" | "best" };
    parallel?: boolean;                      // threads build: use the
                                             // parallel evaluator (set
                                             // once initThreadPool is up)
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

### Fetching only what the style needs

Two prepass calls let a host fetch the tiles and glyph ranges the recipe
actually reads, instead of a fixed 3×3 window and a hand-rolled scan of
every string in the MVT:

```js
r.bindSource("basemap", await fetchMvt(z, x, y));
for (const [dx, dy] of r.requestedNeighborOffsets("basemap")) {
  const nb = await fetchMvt(z, x + dx, y + dy);
  if (nb) r.bindSource("basemap", nb, { coord: [dx, dy] });
}
for (const [src, starts] of Object.entries(r.neededGlyphRanges())) {
  for (const s of starts) {
    r.bindSource(src, await fetchGlyphs(src, `${s}-${s + 255}`));
  }
}
const png = r.renderTile(z, x, y);
```

`neededGlyphRanges` over-approximates on purpose: it lists a range when
any feature in a text layer carries that codepoint in a property the
layer's `text` expression reads (`["get", "name"]` and friends), without
evaluating filters, zoom ranges, or the expression itself, and it lists
it for every fontstack in that layer's fallback chain. It never omits a
range a label needs; it may name a few that go unused. A `text` built
from something other than a property read contributes only its literal
strings.

#### Binding a subset instead of whole ranges

A range holds 256 codepoints and a tile draws a handful of them, so on
CJK labels the loop above spends tens of megabytes to draw a few
thousand glyphs. A host that can assemble a glyph PBF itself — from a
font, or by repacking ranges it holds server-side — should ask for the
codepoints instead and bind one subset per fontstack:

```js
for (const [src, codepoints] of Object.entries(r.neededCodepoints())) {
  r.bindSource(src, await buildSubsetPbf(src, codepoints));
}
```

`bindSource` files each glyph under its own `id`, so a subset may span
any number of ranges in a single message; the fontstack message's
`range` string is metadata and is not used to place glyphs. Repeated
binds accumulate — partial messages for the same range widen it rather
than replacing it — so a host may also split the subset however it
likes across calls.

The one behavioural difference: a codepoint the host does not ship is
simply absent, and its label drops that character. With whole ranges,
binding the range covering a codepoint the font has no glyph for gives
the same result, so this only matters if the subset builder and
`neededCodepoints` disagree.

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
| `OutOfMemory`  | The wasm heap could not grow — the render needs more memory than the host allows |

`OutOfMemory` is thrown from wherever the allocation failed, which means
**the module instance is finished**: the exception unwinds the wasm
frames without running any Rust cleanup, so locks stay locked and
partially built values leak. Discard the instance and re-instantiate if
you want to retry (with a smaller `tileSize`, say). Without this the
same failure surfaces as a `RangeError: Invalid array buffer length`
from the generated glue, which names neither the cause nor the call.
It covers allocation failure only — a host that kills the isolate for
exceeding a cap, rather than refusing to grow the heap, still ends the
instance with no warning.

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

// `new Renderer(...)` starts with an empty asset bank — nothing is
// bundled. Declare each brush/image as a source in the style and supply
// its bytes with `bindSource` (keyed by the source's `src`) before you
// render.
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

### Threads (multithreaded rendering)

An optional third flavor renders tiles across Web Workers using
[`wasm-bindgen-rayon`](https://github.com/RReverser/wasm-bindgen-rayon)
and ezu's ready-set parallel evaluator. It is **off by default** and has
extra requirements — a nightly toolchain and a cross-origin-isolated
page — so the scalar/SIMD builds above stay on stable and run anywhere.

Requirements:

- **Nightly Rust with `rust-src`** — the build rebuilds `std` with
  atomics (`-Z build-std`), which is nightly-only:
  ```sh
  rustup toolchain install nightly --component rust-src
  ```
- **`wasm-bindgen` CLI** on `PATH` (matching the crate's `wasm-bindgen`
  version), and optionally `wasm-opt`.
- **A cross-origin-isolated page** at runtime: the HTML document must be
  served with `Cross-Origin-Opener-Policy: same-origin` and
  `Cross-Origin-Embedder-Policy: require-corp`, which requires HTTPS (or
  localhost). Without isolation, `SharedArrayBuffer` — and therefore the
  worker pool — is unavailable, and the renderer stays single-threaded.

Build it (drops into `target/wasm/threads/` next to the other two):

```sh
./scripts/build-wasm-threads.sh
```

The script runs, in effect:

```sh
RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals \
  -C link-arg=--shared-memory -C link-arg=--import-memory \
  -C link-arg=--max-memory=1073741824 \
  -C link-arg=--export=__heap_base -C link-arg=--export=__wasm_init_tls \
  -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align \
  -C link-arg=--export=__tls_base" \
  cargo +nightly build -p ezu-wasm --release \
    --target wasm32-unknown-unknown --features threads \
    -Z build-std=panic_abort,std

wasm-bindgen target/wasm32-unknown-unknown/release/ezu_wasm.wasm \
  --target web --out-dir target/wasm/threads --out-name ezu_wasm

wasm-opt -O3 --enable-threads --enable-bulk-memory --enable-mutable-globals \
  target/wasm/threads/ezu_wasm_bg.wasm -o target/wasm/threads/ezu_wasm_bg.wasm
```

The output includes a `snippets/` directory with the worker bootstrap —
serve it alongside `ezu_wasm.js`.

Usage from JS:

```js
import init, { Renderer, threadsEnabled, initThreadPool } from "./ezu_wasm.js";

await init();
let parallel = false;
// Spin up the pool once, only when the page is cross-origin isolated.
if (threadsEnabled() && self.crossOriginIsolated) {
  await initThreadPool(navigator.hardwareConcurrency);
  parallel = true;
}

const r = new Renderer(styleJson);
r.bindSource("basemap", mvt);
// The renderer uses the parallel evaluator only when `parallel` is set —
// pass it exactly when the pool is up. Output is byte-for-byte identical
// to the single-threaded path (the evaluator is deterministic).
const rgba = r.renderTile(z, x, y, { format: "rgba", parallel });
```

`threadsEnabled()` reports whether this build has thread support, so one
codebase can load whichever flavor the environment allows.

> **Deployment note.** Cross-origin isolation is not available
> everywhere. Static hosts that can't set COOP/COEP response headers —
> or edge runtimes such as Cloudflare Workers/Pages without extra
> configuration — can't run the threads build; ship the scalar or SIMD
> flavor there and use `threadsEnabled()` / `self.crossOriginIsolated`
> to pick at load time.

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
| `GET /wasm/{scalar\|simd\|threads}/ezu_wasm.js` | This crate's build outputs |
| `GET /wasm-demo/` | This `examples/wasm-demo/` directory |

`ezu serve` sets `COOP: same-origin` + `COEP: require-corp` on the
`/wasm-demo/` and `/wasm/*` routes only, so the demo page is
cross-origin isolated (the threads flavor needs it) while the editor and
tile endpoints are untouched. The `threads` build appears in the demo's
build picker when `target/wasm/threads/` exists; on a page that isn't
isolated it labels itself and falls back to single-threaded rendering.

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
