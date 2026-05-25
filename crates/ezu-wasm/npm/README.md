# ezu

Painterly map renderer compiled to WebAssembly. The JS side owns all I/O
(HTTP, PMTiles, asset fetching); this package exposes a stateful
`Renderer` that holds a parsed style document, its built graph, and an
in-memory brush bank — it renders one tile at a time from raw MVT bytes.

## Install

```sh
npm install ezu
```

## Usage

The package ships three builds and picks one automatically via the
`exports` map:

| Runtime              | Build used  |
| -------------------- | ----------- |
| Vite / Webpack / Rollup | `bundler` |
| Browser (native ESM) | `web`       |
| Node.js              | `nodejs`    |
| Cloudflare Workers   | `bundler` (via `workerd` condition) |

### Bundler / Cloudflare Workers / Node.js

```js
import { Renderer } from "ezu";

const renderer = new Renderer(styleJson);
const png = renderer.render(mvtBytes, z, x, y);
```

### Browser (native ESM, no bundler)

```js
import init, { Renderer } from "ezu/web";

await init();
const renderer = new Renderer(styleJson);
```

### Explicit build selection

If the auto-resolved entry doesn't fit your toolchain, import a build
directly: `ezu/web`, `ezu/bundler`, or `ezu/nodejs`.

## API

```ts
function simdEnabled(): boolean;

class Renderer {
  constructor(styleJson: string);

  setStyle(styleJson: string): number;
  registerBrush(name: string, mybJson: string): void;
  unregisterBrush(name: string): boolean;
  clearBrushes(): void;
  brushCount(): number;
  readonly tileSize: number;

  render(mvtBytes: Uint8Array | null, z: number, x: number, y: number): Uint8Array;        // PNG
  renderWebp(mvtBytes: Uint8Array | null, z: number, x: number, y: number): Uint8Array;    // lossless WebP
  renderRgba(mvtBytes: Uint8Array | null, z: number, x: number, y: number): Uint8Array;    // straight RGBA

  renderAt(mvtBytes: Uint8Array | null, z: number, x: number, y: number,
           tileSize: number, pad: number): Uint8Array;
  renderWebpAt(mvtBytes: Uint8Array | null, z: number, x: number, y: number,
               tileSize: number, pad: number): Uint8Array;
  renderRgbaAt(mvtBytes: Uint8Array | null, z: number, x: number, y: number,
               tileSize: number, pad: number): Uint8Array;

  free(): void;
}
```

Pass `null` for `mvtBytes` to render the style's paper background only
(out-of-range tiles, archive misses, etc.).

Errors are thrown as JavaScript `Error` instances whose `.name`
discriminates the kind: `InvalidStyle`, `BrushParse`, `MvtDecode`,
`RenderFailed`, `PngEncode`, `WebpEncode`.

## License

MIT OR Apache-2.0
