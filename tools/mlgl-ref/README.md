# mlgl-ref — MapLibre reference tile renderer

Rasterizes a single XYZ tile from a MapLibre GL style using
**maplibre-gl-js** inside **headless Chromium** (Playwright), with WebGL
backed by **SwiftShader** (software GL) — so it runs without a real GPU or
a display server. This is the ground-truth image source for
[`ezu-compare`](../../crates/ezu-compare), which pixel-compares ezu's CPU
render against it.

> Note: this is maplibre-gl-**js**, not maplibre-gl-**native**. The two
> share the style spec and rendering intent; expect sub-pixel differences
> in antialiasing and text rasterization. Building maplibre-native from
> source on macOS pulls in cmake + bazel + the full Xcode Metal toolchain,
> so we render the JS engine headless instead. `ezu-compare` treats the
> reference as an opaque PNG directory, so a native generator can be
> swapped in later without touching the comparison code.

## Setup

```sh
cd tools/mlgl-ref
npm install
npx playwright install chromium
```

## Render one tile

```sh
node render.mjs <style-url-or-path> <z> <x> <y> <out.png> [size=512]

# e.g. tile 2/2/1 of the MapLibre demo style
node render.mjs https://demotiles.maplibre.org/style.json 2 2 1 ref.png
```

The tile centre (lon/lat) and `zoom = z` are computed so a `size × size`
viewport frames exactly that XYZ tile. The map is rendered with
`preserveDrawingBuffer` and captured after the `idle` event.
