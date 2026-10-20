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
  readonly warnings: string[];  // what building the style's graph warned about

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

  // Every source the style declares, in declaration order — what to
  // fetch, what kind it is, and whether the binding has to be repeated
  // per tile. Walk this rather than parsing the style yourself; `url` is
  // absent only for inline `geojson`, which needs no binding at all, and
  // a `glyphs` url comes back with `{fontstack}` already substituted.
  sources(): Array<{
    name: string;
    type: "mvt" | "pmtiles" | "dem" | "raster" | "geojson"
        | "sprite" | "font" | "glyphs" | "brush" | "image";
    tileScoped: boolean;
    url?: string;
    indexUrl?: string;          // a sprite index that is a URL, not inline
  }>;

  // Cap the resident glyph bytes per fontstack. 0xFFFFFFFF lifts the cap
  // (and reads back as `Infinity` from `memoryUsage().glyphBudget`).
  setGlyphBudget(bytes: number): void;

  // Let the render cache hold `bytes` of pixels instead of its default
  // (8 MiB); clearCacheBudget() goes back. Kept through setStyle.
  setCacheBudget(bytes: number): void;
  clearCacheBudget(): void;

  // Single unified render. `opts.format` picks the encoder; `tileSize`
  // / `pad` override the canvas for hi-DPI / preview.
  renderTile(z: number, x: number, y: number, opts?: {
    format?: "png" | "webp" | "rgba";      // default "png"
    tileSize?: number;
    pad?: number;
    // Render-time overrides for the style's declared `params`, validated
    // against the declarations (type, min/max) — an unknown name or an
    // out-of-range value throws. Names left out keep their default. One
    // built graph serves every combination.
    params?: Record<string, number | boolean | string>;
    png?: { compression?: "fast" | "default" | "best" };
  }): Uint8Array;

  // Decode a tile-scoped payload (mvt, pmtiles, dem, raster, remote
  // geojson) once, to sit in the set of every render that reads it.
  loadSource(name: string, bytes: Uint8Array): SourceData;

  // renderTile, from a set of loaded data instead of what is bound. Does
  // not trim glyphs to the budget; call trimGlyphs() after a batch.
  renderTileWith(z: number, x: number, y: number, set: SourceSet,
                 opts?: object /* renderTile's opts */): Uint8Array;

  // renderTileWith, also handing back what the named features nodes held:
  // one GeoJSON FeatureCollection per node, in tile units (0..extent).
  renderTileWithFeatures(z: number, x: number, y: number, set: SourceSet,
                         nodes: string[], opts?: object): TileWithFeatures;

  // neededCodepoints / neededGlyphRanges for the features in a set.
  neededCodepointsIn(set: SourceSet): Record<string, number[]>;
  neededGlyphRangesIn(set: SourceSet): Record<string, number[]>;

  // Bring the glyph banks back under setGlyphBudget's budget.
  trimGlyphs(): void;

  // Render every tile at `size` px instead of the style's tile-size,
  // without editing the style, until `clearTileSize()` or `setStyle`.
  setTileSize(size: number): void;
  clearTileSize(): void;

  // Print frames — see "A page at a print scale" below.
  computeFrame(req: FrameRequest): Frame;
  renderFrame(
    frame: Frame,
    sources: ((tile: { z: number; x: number; y: number },
               binder: FrameBinder) => SourceSet | Promise<SourceSet>)
      | null | undefined,
    opts?: { pad?: number; params?: Record<string, number | boolean | string> },
  ): Promise<Uint8Array>;                    // straight RGBA, width × height
  // renderFrame, also gathering features nodes over the page: each node's
  // page as the GeoJSON text `ezu frame --features` writes.
  renderFrameWithFeatures(
    frame: Frame, sources: /* as renderFrame */ any,
    request: { nodes: { node: string; key?: string[] }[];
               coords?: "lonlat" | "px" },
    opts?: { pad?: number; params?: Record<string, number | boolean | string> },
  ): Promise<{ image: Uint8Array; features: { node: string; geojson: string }[] }>;

  free(): void;
}

// One decoded tile. Sets that hold it keep it alive.
class SourceData {
  readonly source: string;                   // the sources.<name> it is for
  free(): void;
}

// The tile data one render reads, each at its place in the 3×3.
class SourceSet {
  constructor();
  bind(data: SourceData,
       opts?: { coord?: [number, number]; sourceZoom?: number }): void;
  contains(source: string): boolean;
  isEmpty(): boolean;
  clone(): SourceSet;                        // shares the data
  free(): void;
}

// Handed to renderFrame's sources: bindSource into the renderer (glyphs,
// fonts, sprites — not tile data, which goes in the set) for that tile
// only, until its sources returns or settles (then it throws StaleBinder).
class FrameBinder {
  bindSource(name: string, bytes: Uint8Array,
             opts?: { coord?: [number, number]; sourceZoom?: number }): void;
}

interface FrameRequest {
  centerLon: number; centerLat: number;      // degrees, WGS84
  scale: number;                             // 1000 for 1:1000
  dpi: number;
  width: number; height: number;             // output pixels
  zoom?: number;                             // the CLI's --zoom
  styleTileSize?: number;                    // default: the style's
}

interface Frame {
  zoom: number; tileSize: number;            // what the tiles render at
  scale: number;                             // the denominator met
  width: number; height: number;
  left: number; top: number;                 // corner, in world pixels
  shift: [number, number];                   // rounding, within ±0.5 px
  tilesX: [number, number];                  // start inclusive, end exclusive
  tilesY: [number, number];
}
```

## A page at a print scale

A printed page is asked for as "1:1000 at 200 dpi, 245 × 174 mm, centred
here", not as a zoom. `computeFrame` picks the zoom and tile size that
meet the scale and the tiles the page covers; `renderFrame` renders them,
calling you back for the `SourceSet` each one renders from, and crops them
into one RGBA page — the same page `ezu frame` writes, pixel for pixel:

```js
const mm = (length, dpi) => Math.round(length / 25.4 * dpi);
const f = renderer.computeFrame({
  centerLon: 139.70, centerLat: 35.68,
  scale: 1000, dpi: 200,
  width: mm(245, 200), height: mm(174, 200),   // 1929 × 1370
});
const loaded = new Map();                      // each fetched tile, decoded once
const rgba = await renderer.renderFrame(f, async ({ z, x, y }, binder) => {
  const key = `${z}/${x}/${y}`;
  if (!loaded.has(key)) {
    loaded.set(key, renderer.loadSource("basemap", await fetchMvt(z, x, y)));
  }
  const set = new SourceSet();
  set.bind(loaded.get(key));                   // and the neighbours, at their coord
  return set;
});
```

A page's tiles border each other, so a `SourceData` kept for the tiles
around it is decoded once however many of them read it. Until the promise
settles the renderer is the frame's: glyphs a set needs are bound through
the `binder` you are handed, which works only for its own tile and only
until that call returns or settles. Calls that would change the renderer —
`bindSource`, `clearSources`, `renderTile`, `trimGlyphs`, another
`renderFrame`, `setStyle`, `setTileSize`, `clearTileSize`,
`setGlyphBudget` — throw `FrameInProgress` meanwhile, so another request
cannot change the frame's tiles under it. `loadSource`, `renderTileWith`
and queries that only read, such as `sourceTile` and `neededCodepointsIn`,
still work.

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
| `FrameInProgress` | A call would change a renderer that a `renderFrame` holds until its promise settles |
| `StaleBinder`  | A frame's `binder` was used after its tile's `sources` returned or settled |

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
