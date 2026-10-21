// `opts.pixelRatio` renders the same tile at a higher density: twice the
// pixels on each side, with the style's pixel widths doubled with them.
//
// Run with scripts/test-wasm-js.sh, which builds the module and points
// EZU_WASM_NODE at it.

import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { test } from "node:test";

const require = createRequire(import.meta.url);
const { Renderer } = require(
  process.env.EZU_WASM_NODE ?? "../../../target/ezu-wasm-node/ezu.js",
);

// A 4 px wide horizontal line across a 32 px tile.
const style = JSON.stringify({
  name: "ratio",
  "tile-size": 32,
  nodes: {
    line: { op: "literal-geometry", lines: [[[0, 2048], [4096, 2048]]] },
    ink: { op: "stroke", features: "@line", "width-px": 4, color: "#000000" },
    bg: { op: "solid", color: "#ffffff" },
    out: { op: "stack", layers: ["@bg", "@ink"] },
  },
  output: "@out",
});

// How many rows of the middle column are mostly ink.
function inkRows(rgba, size) {
  const x = size >> 1;
  let rows = 0;
  for (let y = 0; y < size; y++) {
    if (rgba[(y * size + x) * 4] < 128) rows++;
  }
  return rows;
}

test("a pixel ratio of 2 doubles the tile and the line width", () => {
  const r = new Renderer(style);
  const one = r.renderTile(0, 0, 0, { format: "rgba" });
  const two = r.renderTile(0, 0, 0, { format: "rgba", pixelRatio: 2 });
  assert.equal(one.length, 32 * 32 * 4);
  assert.equal(two.length, 64 * 64 * 4);
  assert.equal(inkRows(one, 32), 4);
  assert.equal(inkRows(two, 64), 8);
  assert.deepEqual(r.renderTile(0, 0, 0, { format: "rgba", pixelRatio: 1 }), one);
});

test("a pixel ratio outside 1 to 4 is refused", () => {
  const r = new Renderer(style);
  for (const pixelRatio of [0.5, 5]) {
    assert.throws(() => r.renderTile(0, 0, 0, { pixelRatio }), { name: "InvalidStyle" });
  }
});
