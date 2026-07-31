# @reearth/ezu

Painterly map renderer compiled to WebAssembly. The JS side owns all I/O
(HTTP, PMTiles, asset fetching); this package exposes a stateful
`Renderer` that holds a parsed style document, its built graph, an
in-memory brush bank, and a per-tile binding buffer that mirrors the
style's `sources` block.

## Install

```sh
npm install @reearth/ezu
```

## Usage

The package ships four builds and picks one automatically via the
`exports` map:

| Runtime              | Build used  |
| -------------------- | ----------- |
| Vite / Webpack / Rollup | `bundler` |
| Browser (native ESM) | `web`       |
| Node.js              | `nodejs`    |
| Cloudflare Workers   | `workerd` (via the `workerd` condition) |

Every build also exists in a wasm SIMD128 flavor under
`@reearth/ezu/simd` — see [SIMD](#simd).

### Bundler / Cloudflare Workers / Node.js

```js
import { Renderer } from "@reearth/ezu";

const renderer = new Renderer(styleJson);
renderer.bindSource("basemap", mvtBytes);
const png = renderer.renderTile(z, x, y);
renderer.clearSources();
```

### Browser (native ESM, no bundler)

```js
import init, { Renderer } from "@reearth/ezu/web";

await init();
const renderer = new Renderer(styleJson);
```

### Explicit build selection

If the auto-resolved entry doesn't fit your toolchain, import a build
directly: `@reearth/ezu/web`, `@reearth/ezu/bundler`, `@reearth/ezu/nodejs`,
or `@reearth/ezu/workerd`.

`workerd` reuses the `bundler` glue but instantiates the wasm itself,
because Cloudflare Workers resolve a `.wasm` import to an uninstantiated
`WebAssembly.Module` rather than to live exports. Importing
`@reearth/ezu/bundler` on Workers therefore fails with
`__wbindgen_start is not a function`.

The raw artifacts are exported too, for hosts that need to wire up
instantiation themselves: `@reearth/ezu/bundler/ezu_bg.js`,
`@reearth/ezu/bundler/ezu_bg.wasm`, and `@reearth/ezu/web/ezu_bg.wasm`.

## SIMD

The same four builds are shipped a second time compiled with
`-C target-feature=+simd128`, under the `simd` subpath. It resolves by
the same conditions as the default entry, so one import covers every
runtime:

```js
import { Renderer } from "@reearth/ezu/simd";
```

The default entry (`.`) stays **non-SIMD** on purpose. Safari gained
wasm SIMD128 only in 16.4, and a module that uses SIMD instructions
fails to *compile* on older engines rather than degrading — so making
SIMD the package default would hard-break those browsers. Opt in where
you know the engine supports it: Cloudflare Workers (workerd), Node 16+,
and Chrome/Firefox/Safari 16.4+.

`simdEnabled()` reports which flavor is loaded — `true` only in the
`simd` builds:

```js
import { simdEnabled } from "@reearth/ezu/simd";
simdEnabled(); // true
```

Individual builds and raw artifacts mirror the default layout:
`@reearth/ezu/simd/web`, `/simd/bundler`, `/simd/nodejs`,
`/simd/workerd`, `@reearth/ezu/simd/bundler/ezu_bg.js`,
`@reearth/ezu/simd/bundler/ezu_bg.wasm`, and
`@reearth/ezu/simd/web/ezu_bg.wasm`.

A future version could ship a single entry that feature-detects SIMD at
load time and instantiates the matching wasm; that would double the
bytes a browser may download, so for now the choice is explicit.

## API

```ts
function simdEnabled(): boolean;

class Renderer {
  constructor(styleJson: string);

  setStyle(styleJson: string): number;            // → new node count
  readonly tileSize: number;

  // Bind tile bytes under a `sources.<name>` entry from the style.
  // The renderer dispatches on the source's declared `type`:
  //   - brush  → parse `.myb` JSON, persistent
  //   - image  → decode PNG/WebP, persistent
  //   - mvt/pmtiles → decode at render time, cleared by `clearSources`
  //   - dem    → decode + stitch at render time. Pass each neighbour
  //              tile with `{ coord: [dx, dy] }` (dx, dy ∈ {-1, 0, 1}).
  bindSource(name: string, bytes: Uint8Array,
             opts?: { coord?: [number, number] }): void;
  clearSources(): void;
  boundSources(): string[];     // names with at least one pending tile-scoped binding

  // Single unified render. `opts.format` picks the encoder; `tileSize`
  // / `pad` override the canvas for hi-DPI / preview.
  renderTile(z: number, x: number, y: number, opts?: {
    format?: "png" | "webp" | "rgba";      // default "png"
    tileSize?: number;
    pad?: number;
    png?: { compression?: "fast" | "default" | "best" };
  }): Uint8Array;

  free(): void;
}
```

## Output formats

- **`format: "png"`** (default) returns PNG bytes — feed to `<img>`
  via `URL.createObjectURL(new Blob([buf], { type: "image/png" }))`.
- **`format: "webp"`** returns lossless WebP bytes (~15 % smaller
  than PNG on painterly tiles). Pure-Rust encoder, no native deps.
- **`format: "rgba"`** returns straight (un-premultiplied) 8-bit
  RGBA bytes (`tile_size * tile_size * 4`) — feed directly to
  `ctx.putImageData(new ImageData(new Uint8ClampedArray(buf.buffer), w, h), 0, 0)`
  and skip the PNG decode round trip.

WebP is lossless via `image-webp` and has no quality knob — for
lossy WebP without C deps, render as `"rgba"` then call
`OffscreenCanvas.convertToBlob({ type: "image/webp", quality })`.

## Missing tiles

Don't bind any MVT source for that tile — `renderTile` returns the
style's paper background. `features` source nodes see no
`tile.<layer>` binding and emit an empty layer, so downstream paint
nodes short-circuit.

## Errors

Every fallible method throws a JavaScript `Error` whose `.name`
discriminates the kind:

| `.name` | When |
|---|---|
| `InvalidStyle` | `new Renderer(...)` / `setStyle(...)` rejected the JSON |
| `BrushParse`   | `registerBrush(...)` or `bindSource` on a brush source couldn't parse the `.myb` JSON |
| `MvtDecode`    | `bindSource` (mvt/pmtiles) or render couldn't decode the MVT bytes |
| `DemDecode`    | `bindSource` (dem) couldn't decode the raster-DEM tile |
| `UnknownSource`| `bindSource` was given a name not in the style's `sources` block, or `coord` was malformed |
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

## License

MIT OR Apache-2.0
