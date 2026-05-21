# ezu-wasm

WebAssembly bindings for the `ezu` painterly map renderer.

The JS side owns all I/O (HTTP, PMTiles, asset fetching). This crate exposes
a stateful [`Renderer`](src/lib.rs) that holds a parsed Ezu Style document
plus a brush bank, and renders one tile at a time from raw MVT bytes.

## API

```ts
class Renderer {
  constructor(styleJson: string);
  setStyle(styleJson: string): number;            // → new layer count
  registerBrush(name: string, mybJson: string): void;
  brushCount(): number;
  render(mvtBytes: Uint8Array, z: number, x: number, y: number): Uint8Array;  // PNG
  renderBlank(): Uint8Array;                       // paper-only tile
  readonly tileSize: number;
}

function simdEnabled(): boolean;                   // build was compiled with +simd128
```

`render()` and `renderBlank()` return PNG-encoded bytes sized to
`style["tile-size"]`. `registerBrush` accepts MyPaint `.myb` JSON content
verbatim. Layer references like `"@watercolor_glazing"` look up the bank
entry `"watercolor_glazing"` (the `@` prefix is stripped).

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

A self-contained demo lives at [`www/index.html`](www/index.html). It picks
between the scalar and SIMD builds at runtime, renders a single MVT tile on a
`<canvas>`, and runs a bench-of-N for timing. It needs three HTTP routes that
the [`ezu-server`](../ezu-server) crate provides out of the box:

| Path | Source |
|---|---|
| `GET /style` | Current Ezu Style JSON |
| `GET /mvt/{z}/{x}/{y}` | Raw decompressed MVT bytes |
| `GET /assets/brushes/*.myb` | MyPaint brush files |
| `GET /wasm/{scalar\|simd}/ezu_wasm.js` | This crate's wasm-pack outputs |
| `GET /wasm-demo/` | This `www/` directory |

To run end-to-end from a clean tree:

```sh
# 1. Build both WASM flavors (see above)
# 2. Start the server (serves the editor, the static dirs, and the demo)
cargo run --release -p ezu-server
# 3. Open http://127.0.0.1:8080/wasm-demo/
```

## Benchmark (M-series Mac, Chrome)

Same tile (`z=13`, central Tokyo, `7276/3225`), 12-sample median, MVT cached
client-side so timings reflect pure WASM `render()`:

| Build | binary | v128 ops | min | **p50** | mean | max |
|---|---|---|---|---|---|---|
| Scalar | 729 KB | 0 | 1113 | **1114** ms | 1117 | 1150 |
| SIMD (`+simd128`) | 634 KB | 13,367 | 1013 | **1015** ms | 1016 | 1026 |

SIMD wins **~1.10×** on this watercolor style. The bulk of the work is
hokusai's `Brush::stroke_to` (`libmypaint` emulation, mostly scalar f32
dynamics), so SIMD has only narrow surface area to vectorize. Heavier
dab-fill or blur layers will show a larger gap.

Native (release binary, same workload, same machine): ~280 ms. WASM is
~3.6× slower than native here — consistent with the libmypaint kernel
being the hot loop.

## Caveats

- `wasm32-unknown-unknown` has no monotonic clock, so any
  `std::time::Instant::now()` call panics at runtime. `ezu-paint`'s
  per-layer trace is gated on `target_arch = "wasm32"`; if you add more
  timing in the rendering crates, gate it the same way.
- This crate isn't published to crates.io (`publish = false`).
- The `panic-hook` feature is on by default and forwards panics to
  `console.error`. Turn it off for tiny size-sensitive builds.

## License

Same as the rest of the workspace: MIT or Apache-2.0, at your option.
