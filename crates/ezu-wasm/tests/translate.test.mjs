// MapLibre translation from JS: `translateMapLibre` answers with a style
// the renderer builds from, the translator's warnings beside it, and the
// options the CLI's `ezu translate` takes.
//
// Run with scripts/test-wasm-js.sh, which builds the module and points
// EZU_WASM_NODE at it.

import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { test } from "node:test";

const require = createRequire(import.meta.url);
const { Renderer, translateMapLibre } = require(
  process.env.EZU_WASM_NODE ?? "../../../target/ezu-wasm-node/ezu.js",
);

const style = JSON.stringify({
  version: 8,
  sources: {
    basemap: { type: "vector", tiles: ["https://example.invalid/{z}/{x}/{y}.mvt"] },
  },
  layers: [
    { id: "bg", type: "background", paint: { "background-color": "#f0ead8" } },
    {
      id: "water",
      type: "fill",
      source: "basemap",
      "source-layer": "water",
      paint: { "fill-color": "#9cc0e0" },
    },
    {
      id: "hidden",
      type: "line",
      source: "basemap",
      "source-layer": "roads",
      layout: { visibility: "none" },
    },
    { id: "sky", type: "sky" },
  ],
});

test("a translated style builds a renderer", () => {
  const { recipe, warnings } = translateMapLibre(style);
  assert.equal(recipe["tile-size"], 512);
  assert.ok(warnings.some((w) => w.includes("sky")), `no warning for the sky layer in ${warnings}`);
  const r = new Renderer(JSON.stringify(recipe));
  assert.equal(r.tileSize, 512);
});

test("options reach the translator", () => {
  const plain = translateMapLibre(style).recipe;
  const kept = translateMapLibre(style, { tileSize: 256, pad: 0, keepHidden: true }).recipe;
  assert.equal(kept["tile-size"], 256);
  assert.equal(kept.pad, 0);
  assert.ok(
    Object.keys(kept.nodes).length > Object.keys(plain.nodes).length,
    "keepHidden kept no more nodes",
  );
});

test("a refused style or option throws InvalidStyle", () => {
  for (const [input, options] of [
    ["not json", undefined],
    [JSON.stringify({ version: 8, sources: {} }), undefined],
    [style, { tileSize: 0 }],
    [style, { migrate: "yes" }],
    [style, { fonts: { "Noto Sans Regular": 1 } }],
  ]) {
    assert.throws(() => translateMapLibre(input, options), { name: "InvalidStyle" });
  }
});
